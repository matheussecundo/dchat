// Run: node --test worker/
import { test, beforeEach, afterEach } from "node:test";
import assert from "node:assert/strict";
import worker from "./index.js";
import { iceServers, withoutPort53, TTL_SECONDS } from "./ice.js";

const CF_RESPONSE = {
  iceServers: [
    { urls: ["stun:stun.cloudflare.com:3478", "stun:stun.cloudflare.com:53"] },
    {
      urls: [
        "turn:turn.cloudflare.com:3478?transport=udp",
        "turn:turn.cloudflare.com:53?transport=udp",
        "turns:turn.cloudflare.com:443?transport=tcp",
      ],
      username: "user",
      credential: "secret",
    },
  ],
};

const env = (overrides = {}) => ({
  TURN_KEY_ID: "key123",
  TURN_KEY_API_TOKEN: "token456",
  ICE_LIMITER: { limit: async () => ({ success: true }) },
  ASSETS: { fetch: async () => new Response("asset") },
  ...overrides,
});

const get = (headers = {}) =>
  new Request("https://dchat.example.workers.dev/ice-servers", { headers: { "Sec-Fetch-Site": "same-origin", ...headers } });

let calls;
const realFetch = globalThis.fetch;
beforeEach(() => {
  calls = [];
  globalThis.fetch = async (url, init) => {
    calls.push({ url, init });
    return new Response(JSON.stringify(CF_RESPONSE), { status: 201 });
  };
});
afterEach(() => {
  globalThis.fetch = realFetch;
});

test("returns fresh credentials from the TURN API, without port 53 and uncached", async () => {
  const res = await iceServers(get(), env());
  assert.equal(res.status, 200);
  assert.equal(res.headers.get("Cache-Control"), "no-store");
  const body = await res.json();
  assert.deepEqual(body.iceServers[1].urls, [
    "turn:turn.cloudflare.com:3478?transport=udp",
    "turns:turn.cloudflare.com:443?transport=tcp",
  ]);
  assert.equal(body.iceServers[1].credential, "secret");

  assert.equal(calls.length, 1);
  assert.equal(calls[0].url, "https://rtc.live.cloudflare.com/v1/turn/keys/key123/credentials/generate-ice-servers");
  assert.equal(calls[0].init.method, "POST");
  assert.equal(calls[0].init.headers.Authorization, "Bearer token456");
  assert.deepEqual(JSON.parse(calls[0].init.body), { ttl: TTL_SECONDS });
});

test("the API token never appears in the response", async () => {
  const res = await iceServers(get(), env());
  assert.ok(!(await res.text()).includes("token456"));
});

test("rejects other methods and cross-site requests", async () => {
  const post = new Request("https://dchat.example.workers.dev/ice-servers", { method: "POST" });
  assert.equal((await iceServers(post, env())).status, 405);
  assert.equal((await iceServers(get({ "Sec-Fetch-Site": "cross-site" }), env())).status, 403);
  assert.equal(calls.length, 0);
});

test("404 when TURN is not configured, so the app falls back to STUN", async () => {
  const res = await iceServers(get(), env({ TURN_KEY_API_TOKEN: undefined }));
  assert.equal(res.status, 404);
  assert.equal(calls.length, 0);
});

test("429 when the per-IP limit is exceeded, keyed by the client IP", async () => {
  let key;
  const limiter = { limit: async (opts) => ((key = opts.key), { success: false }) };
  const res = await iceServers(get({ "CF-Connecting-IP": "203.0.113.7" }), env({ ICE_LIMITER: limiter }));
  assert.equal(res.status, 429);
  assert.equal(key, "203.0.113.7");
  assert.equal(calls.length, 0);
});

test("502 when the TURN API fails", async () => {
  globalThis.fetch = async () => new Response("nope", { status: 500 });
  assert.equal((await iceServers(get(), env())).status, 502);
});

test("everything else is served from static assets", async () => {
  let served;
  const assets = { fetch: async (req) => ((served = new URL(req.url).pathname), new Response("ok")) };
  const res = await worker.fetch(new Request("https://dchat.example.workers.dev/missing.txt"), env({ ASSETS: assets }));
  assert.equal(await res.text(), "ok");
  assert.equal(served, "/missing.txt");
  // Also under a path prefix.
  const ice = await worker.fetch(new Request("https://x.example/app/ice-servers", { headers: { "Sec-Fetch-Site": "same-origin" } }), env());
  assert.equal(ice.status, 200);
});

test("withoutPort53 drops servers left with no usable URL", () => {
  assert.deepEqual(withoutPort53([{ urls: "turn:x:53?transport=udp" }, { urls: "stun:y:3478" }]), [{ urls: ["stun:y:3478"] }]);
});
