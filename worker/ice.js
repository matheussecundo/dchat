// GET /ice-servers: short-lived Cloudflare Realtime TURN credentials (see index.js).
// Kept out of the entry module: the Workers runtime treats every named export of the
// entry module as a handler, so constants and helpers must live here.

const TURN_API = "https://rtc.live.cloudflare.com/v1/turn/keys";
// Longer than any expected call (an expired credential stops refreshing the relay);
// Cloudflare allows up to 48 hours.
export const TTL_SECONDS = 12 * 60 * 60;

export async function iceServers(request, env) {
  if (request.method !== "GET") {
    return text(405, "Method not allowed");
  }
  // Browsers label cross-site requests, so other websites can't spend this TURN quota
  // through their visitors. (Scripts can omit the header; the rate limit covers those.)
  const site = request.headers.get("Sec-Fetch-Site");
  if (site && site !== "same-origin") {
    return text(403, "Forbidden");
  }
  if (!env.TURN_KEY_ID || !env.TURN_KEY_API_TOKEN) {
    // Not configured: the app falls back to STUN (and any &turn= in the room link).
    return text(404, "TURN not configured");
  }
  if (env.ICE_LIMITER) {
    // Per-IP: there is no account to key on. Generous, since mobile users share IPs.
    const key = request.headers.get("CF-Connecting-IP") ?? "unknown";
    const { success } = await env.ICE_LIMITER.limit({ key });
    if (!success) {
      return text(429, "Too many requests");
    }
  }

  const upstream = await fetch(`${TURN_API}/${env.TURN_KEY_ID}/credentials/generate-ice-servers`, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${env.TURN_KEY_API_TOKEN}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ ttl: TTL_SECONDS }),
  });
  if (!upstream.ok) {
    return text(502, "TURN credentials unavailable");
  }
  const { iceServers: servers } = await upstream.json();
  return new Response(JSON.stringify({ iceServers: withoutPort53(servers ?? []) }), {
    headers: {
      "Content-Type": "application/json",
      // Credentials are per request; never cache them anywhere.
      "Cache-Control": "no-store",
    },
  });
}

/** Browsers block port 53, so those TURN URLs only time out: drop them. */
export function withoutPort53(servers) {
  return servers
    .map((server) => {
      const urls = (Array.isArray(server.urls) ? server.urls : [server.urls]).filter(
        (u) => typeof u === "string" && !/:53(\?|$)/.test(u),
      );
      return { ...server, urls };
    })
    .filter((server) => server.urls.length > 0);
}

function text(status, body) {
  return new Response(body, { status, headers: { "Cache-Control": "no-store" } });
}
