// dchat on Cloudflare Workers: serves the static client (crates/client/dist) plus one
// endpoint, GET /ice-servers, which returns short-lived Cloudflare Realtime TURN
// credentials for members whose networks block direct WebRTC links.
//
// The TURN API token never leaves this Worker. The request carries nothing about any
// room: room IDs and keys live in the URL fragment, which browsers never send.

import { iceServers } from "./ice.js";

// Only the default handler may be exported from this entry module (see ice.js).
export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname.endsWith("/ice-servers")) {
      return iceServers(request, env);
    }
    // Only reached for paths that match no static asset.
    return env.ASSETS.fetch(request);
  },
};
