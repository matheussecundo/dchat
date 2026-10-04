# Deploying dchat

dchat is a static web app. What gets deployed is the release build in `crates/client/dist/`: one HTML page, a JS loader, a WebAssembly module, a stylesheet and a service worker. There is no backend to run. Members connect to each other directly, and Nostr relays carry only the encrypted handshake.

The Axum server in `crates/server` is for development only (self-signed certificate, mock relay). Never expose it to the internet.

This guide covers two hosting options:

| | GitHub Pages | Cloudflare Workers |
|---|---|---|
| Cost | Free | Free for the site; TURN free up to 1,000 GB, then $0.05/GB |
| Address | `https://<user>.github.io/<repo>/` or a custom domain | `https://dchat.<subdomain>.workers.dev` or a custom domain |
| TURN for members who can't connect directly | Not included: add your own with `&turn=` in room links | Built in: fresh Cloudflare TURN credentials for every room entry |
| Setup | ~5 minutes | ~15 minutes |

Pick **GitHub Pages** to get online quickly. Pick **Cloudflare** if people will join from mobile data or strict networks, where voice, video and files often need TURN (see README: "When Members Can't Connect Directly (NAT)"). You can also run both.

---

## 1. Put the repository on GitHub

Both options build and deploy from GitHub Actions, so the code must be on GitHub first.

1. Create an empty repository on GitHub (no README or license, so the first push doesn't conflict).
2. Push this repository to it:
   ```bash
   git remote add origin https://github.com/<user>/<repo>.git
   git push -u origin main
   ```

How the workflows fit together (`.github/workflows/`):

| Workflow | Runs | Does |
|---|---|---|
| `build.yml` | Called by the two below | Rust unit tests, Worker unit tests, release build of the client, a check that no test hooks ended up in the bundle; hands the build over as the `site` artifact |
| `pages.yml` | Every push to `main`, or by hand | Builds, then publishes to GitHub Pages |
| `cloudflare.yml` | Every push to `main`, or by hand | Builds, then deploys to Cloudflare; skips the deploy until its secrets are set |

Builds use the committed `Cargo.lock` (`--locked`), so CI ships exactly the dependency versions that were tested.

Using only one host? Disable the other workflow under **Actions** → (workflow name) → **⋯** → **Disable workflow**. Otherwise `pages.yml` fails on every push while Pages is not enabled.

---

## 2. GitHub Pages

### Setup
1. In the repository, open **Settings → Pages**.
2. Under **Build and deployment → Source**, choose **GitHub Actions**.
3. Start a deploy: push to `main`, or open **Actions → Deploy to GitHub Pages → Run workflow**.
4. When the run finishes (about 5–10 minutes the first time, faster later thanks to caching), the address appears in the run summary and under **Settings → Pages**: `https://<user>.github.io/<repo>/`.

### Custom domain (optional)
1. **Settings → Pages → Custom domain**: enter e.g. `chat.example.com` and save.
2. At your DNS provider, add a `CNAME` record from `chat.example.com` to `<user>.github.io`.
3. Once the DNS check passes, tick **Enforce HTTPS**. Browsers only allow camera and microphone access over HTTPS.

### Check it works
- Open the site, create a room and open the invite on a second device on a different network. Both should show **Connected** within a few seconds, and messages should go both ways.
- In the browser's developer tools, **Network** shows one `404` for `ice-servers` each time you enter a room. That is expected on GitHub Pages: there is no TURN endpoint, so the app uses STUN only.

### TURN on GitHub Pages
GitHub Pages can't run server code, so it can't hand out TURN credentials. Members on mobile data or behind strict NATs may get text only. To give them voice, video and files, run a TURN server yourself (for example [coturn](https://github.com/coturn/coturn)) and add it to room links:
```
https://<user>.github.io/<repo>/#room=…&key=…&turn=turns:turn.example.com:5349&turnuser=dchat&turnpass=…
```
Everyone with the link can see those credentials, so give them a dedicated account on your TURN server.

Rooms created with *Hide members' IP addresses* need such a TURN server in the link; without one, nobody can connect.

### STUN on GitHub Pages
Without the Cloudflare endpoint, members use Google's public STUN server (`stun.l.google.com`), which sees their IP addresses. To avoid it, name another STUN server in room links, for example your own coturn, or Cloudflare's public one:
```
https://<user>.github.io/<repo>/#room=…&key=…&stun=stun:stun.cloudflare.com:3478
```

---

## 3. Cloudflare (Workers + TURN)

The Cloudflare setup deploys the same static files as a Worker with static assets (`wrangler.jsonc`), plus a small script (`worker/`) with one endpoint, `GET /ice-servers`. When someone enters a room, the app asks that endpoint for TURN servers. The Worker gets fresh, short-lived credentials from Cloudflare's TURN API and returns them.

Security properties:
- The TURN API token exists only as a Worker secret. It never reaches the browser or the repository.
- Credentials expire after 12 hours and are never cached (`Cache-Control: no-store`; the service worker doesn't touch the endpoint).
- The request carries no room information. The room ID and key live in the URL fragment, which browsers never send.
- Cross-site requests are refused, and each IP may make 20 requests per minute, so other websites can't spend your TURN quota.
- TURN relays encrypted packets only. Cloudflare sees IP addresses and traffic volume, never message or media content.
- The answer also lists Cloudflare's STUN server, so members never contact Google's.
- Rooms created with *Hide members' IP addresses* work out of the box: members connect only through Cloudflare's TURN.

### Setup
You need a Cloudflare account (the free plan is enough).

1. **Create a TURN key.** In the Cloudflare dashboard, open **Realtime → TURN**, create a TURN key and copy its **key ID** and **API token** right away (you may not be able to view the token again). You'll need both in step 5.
2. **Create a deploy token.** Open **My Profile → API Tokens → Create Token**, use the **Edit Cloudflare Workers** template, and copy the token. Also copy your **Account ID**, shown in the dashboard (for example on the **Workers & Pages** overview).
3. **Add GitHub secrets.** In the repository, open **Settings → Secrets and variables → Actions → New repository secret** and add:
   - `CLOUDFLARE_API_TOKEN`: the deploy token from step 2
   - `CLOUDFLARE_ACCOUNT_ID`: your account ID
4. **Deploy.** Push to `main`, or open **Actions → Deploy to Cloudflare → Run workflow**. The first deploy creates the Worker `dchat`. The site is then at `https://dchat.<your-subdomain>.workers.dev`; the address is also shown in the run log and on the Worker's page in the dashboard.
5. **Give the Worker the TURN key.** This is needed once; secrets are kept across deploys. Either:
   - in the dashboard: **Workers & Pages → dchat → Settings → Variables and Secrets → Add**, type **Secret**, names `TURN_KEY_ID` and `TURN_KEY_API_TOKEN`; or
   - from a terminal (Node.js 22+): `npx wrangler login`, then `npx wrangler secret put TURN_KEY_ID` and `npx wrangler secret put TURN_KEY_API_TOKEN`.

Without the TURN secrets the site still works; it just behaves like GitHub Pages (STUN only).

### Custom domain (optional)
If the domain's DNS is on Cloudflare: **Workers & Pages → dchat → Settings → Domains & Routes → Add → Custom domain**. Cloudflare creates the DNS record and the HTTPS certificate.

### Check it works
1. **The endpoint.** Run (the header imitates the app's own request):
   ```bash
   curl -H "Sec-Fetch-Site: same-origin" https://dchat.<your-subdomain>.workers.dev/ice-servers
   ```
   - JSON with an `iceServers` list containing `turn:turn.cloudflare.com…` URLs, a `username` and a `credential`: working.
   - `TURN not configured` (404): step 5 is missing.
   - `TURN credentials unavailable` (502): the key ID or token is wrong.
2. **In the app.** Enter a room with the developer tools open: **Network** shows `ice-servers` with status `200`.
3. **In a call.** Two members on different networks join voice. In Chrome, `chrome://webrtc-internals` lists candidates of type `relay` when TURN is in use. Members who can connect directly still do; TURN is only a fallback.

### Deploy from your own machine (optional)
Requires Node.js 22 or newer (Wrangler 4).
```bash
npx wrangler login
make deploy-cloudflare      # release build + wrangler deploy
```

### Test the Worker locally (optional)
```bash
make build-client
npx wrangler@4 dev          # serves the app and /ice-servers at http://localhost:8787
```
To test TURN locally, put the TURN key in a `.dev.vars` file at the repository root (it is git-ignored):
```
TURN_KEY_ID=...
TURN_KEY_API_TOKEN=...
```
Worker unit tests need no account: `node --test worker/` (or `make test-worker`).

### Costs and limits
- TURN: $0.05 per GB that the TURN server sends to clients, after a free tier of 1,000 GB. Only pairs of members who can't connect directly use it.
- A credential lasts 12 hours (`TTL_SECONDS` in `worker/ice.js`; Cloudflare allows up to 48). A call longer than that loses its relay until the member rejoins the room.
- Rate limit: 20 `/ice-servers` requests per minute per IP (`ratelimits` in `wrangler.jsonc`; the period must be 10 or 60 seconds). Many mobile users share an IP, so don't set it much lower.
- Serving the static site is free on Workers.

### Rotating the TURN key
Create a new TURN key, update the two Worker secrets (step 5), then delete the old key in **Realtime → TURN**. Credentials already issued stop working when they expire or when the old key is deleted.

---

## 4. Nostr relays: public or your own

Members find each other through Nostr relays. Relays only pass along encrypted, signed, short-lived handshake messages tagged with a hash of the room ID; they never see room IDs, keys or chat content. They do see members' IP addresses and timing.

Relays are needed only to find members and to open each direct link. Once two members are linked, everything else between them goes over that link, including setting up voice and video. A relay outage doesn't affect members who are already connected; it only delays newcomers and links that need to reconnect.

By default rooms use the public relays `wss://relay.damus.io`, `wss://nos.lol` and `wss://relay.primal.net`. Running your own relay removes that dependency and keeps the metadata with you.

### Choosing relays per room
When creating a room, **Signaling relays** offers:
- **Public Nostr relays (default)**: nothing is added to the link.
- **My relay**: only the addresses you enter (`wss://…`, comma-separated). The link gets `&relays=wss://relay.example.com`.
- **My relay + public Nostr relays (backup)**: both. The link gets `&relays=wss://relay.example.com,nostr`, where `nostr` stands for the public relays.

Everyone who joins uses the room's relays (they come with the link), so all members can find each other. The **⚡ Nostr** badge shows which relays the room uses. If a relay connection drops, the app reconnects on its own (after 1 s, then backing off up to 30 s).

To pre-fill **My relay** for everyone creating rooms on your site, set a repository variable **Settings → Secrets and variables → Actions → Variables → `DCHAT_RELAY_URL`** (e.g. `wss://relay.example.com`). The deploy workflows build it into the app. Public relays stay the default choice.

### Running dchat-relay
`dchat-relay` (`crates/relay`) is a small relay made for dchat:
- **RAM only:** it forwards ephemeral events and stores nothing. Its logs contain no IP addresses, room topics or content.
- **dchat only:** it accepts only dchat's signaling events (kind 20001) with a room topic, a valid signature and a fresh timestamp (at most 5 minutes old). It is not a general-purpose Nostr relay.
- **Abuse limits:** 128 KiB per message, 8 subscriptions per connection, 300 events burst then 30 per second per connection, 64 connections per IP.
- **Optional origin lock:** accept only your dchat site (`--allowed-origin https://chat.example.com`), so other sites can't use your relay.
- Publishes its limits as a standard NIP-11 information document.

It needs a server with a public IP address and a DNS name pointing at it (e.g. `relay.example.com`), with ports 80 and 443 open. Browsers on an HTTPS site can only use `wss://`, so it runs behind [Caddy](https://caddyserver.com/), which gets and renews the TLS certificate automatically. The files are in `deploy/relay/`.

#### With Docker (recommended)
On the server, with Docker installed:
```bash
git clone https://github.com/<user>/<repo>.git dchat && cd dchat/deploy/relay
RELAY_DOMAIN=relay.example.com ALLOWED_ORIGINS=https://chat.example.com docker compose up -d --build
```
Leave out `ALLOWED_ORIGINS` to accept any site. The first start builds the image (a few minutes); Caddy then obtains the certificate.

Prefer a prebuilt image? `.github/workflows/relay-image.yml` publishes `ghcr.io/<user>/dchat-relay:latest` whenever the relay changes on `main` (make the package public under **Packages** if your server pulls it without logging in). Then:
```bash
RELAY_DOMAIN=relay.example.com RELAY_IMAGE=ghcr.io/<user>/dchat-relay:latest docker compose up -d --no-build
```

Update: `git pull && docker compose up -d --build` (or `docker compose pull && docker compose up -d` with the prebuilt image). Members reconnect automatically; rooms carry on, because established member links are direct.

#### Without Docker
```bash
cargo build -p relay --release --locked          # or: make build-relay
sudo install -m 0755 target/release/dchat-relay /usr/local/bin/
sudo cp deploy/relay/dchat-relay.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now dchat-relay
```
The unit runs it on `127.0.0.1:7447` with systemd sandboxing. Add to `/etc/caddy/Caddyfile`:
```
relay.example.com {
	reverse_proxy 127.0.0.1:7447
}
```
and reload Caddy (`sudo systemctl reload caddy`).

#### Options
| Flag | Environment variable | Default |
|---|---|---|
| `--listen` | `DCHAT_RELAY_LISTEN` | `127.0.0.1:7447` (Docker image: `0.0.0.0:7447`) |
| `--allowed-origin` (repeatable) | `DCHAT_RELAY_ALLOWED_ORIGINS` (comma-separated) | any origin |
| `--trust-proxy` | `DCHAT_RELAY_TRUST_PROXY` | off (on in `docker-compose.yml`: client IPs come from Caddy's `X-Forwarded-For`) |
| `--max-connections-per-ip` | `DCHAT_RELAY_MAX_CONNECTIONS_PER_IP` | `64` |
| `--name` | `DCHAT_RELAY_NAME` | `dchat-relay` |

Only turn on `--trust-proxy` behind a proxy you control: otherwise clients could fake their IP to get around per-IP limits.

#### Check it works
```bash
curl -H "Accept: application/nostr+json" https://relay.example.com/
```
returns the relay's information (`"software":"dchat-relay"`). Then create a room with **My relay** → `wss://relay.example.com`, join from a second device, and check the **⚡ Nostr** badge shows `(1 active)` on both.

#### Other relay software
Any Nostr relay that forwards ephemeral events (kinds 20000–29999) works, for example [strfry](https://github.com/hoytech/strfry) or nostr-rs-relay. They are general-purpose relays, so restrict them to kind 20001 if you only want dchat traffic.

---

## 5. Remote control app (dchat-host)

Remote control needs `dchat-host` on the computer being controlled (Linux or Windows). Build it with `cargo build -p host-agent --release` on that system; the binary is `target/release/dchat-host` (`dchat-host.exe` on Windows). Ready-made downloads come with a later release workflow.

It only accepts your dchat site, so tell it the site's address:
- at run time: `dchat-host --allow-origin https://chat.example.com` (repeatable, or `DCHAT_HOST_ALLOWED_ORIGINS=https://a,https://b`);
- or bake it in at build time: `DCHAT_HOST_ORIGINS=https://chat.example.com cargo build -p host-agent --release`.

On a GitHub Pages site the origin is `https://<user>.github.io` for all of your repositories; the pairing code still protects the app, but a custom domain keeps it to dchat.

On Linux, install `crates/host-agent/dist/60-dchat-host.rules` (see the README) so it can use `/dev/uinput` without root. On Windows, mouse and keyboard need nothing extra; game controllers need the [ViGEmBus driver](https://github.com/nefarius/ViGEmBus/releases) (dchat-host says so at start when it's missing). To control windows of apps running as administrator, run dchat-host as administrator too. It listens on `127.0.0.1:7448` (`--port` to change; the same port goes in dchat's dialog), never on the network.

---

## 6. Troubleshooting

| Symptom | Cause and fix |
|---|---|
| `pages.yml` fails at "configure-pages" | Pages isn't enabled: **Settings → Pages → Source: GitHub Actions**. Or disable the workflow if you only use Cloudflare. |
| `cloudflare.yml` passes but deploys nothing | The `CLOUDFLARE_API_TOKEN` secret isn't set; the run log shows "skipping the Cloudflare deploy". |
| Wrangler says it needs Node.js 22 | Wrangler 4 requires Node.js 22+. CI already uses 22; update Node locally. |
| The page is blank and the console shows 404s for `.js`/`.wasm` files | The build isn't using relative paths. Build from `crates/client` so `Trunk.toml` (`public_url = "./"`) applies. |
| An old version keeps showing after a deploy | The service worker updates in the background: reload once more, or close and reopen the tab. |
| After a deploy, some members can't connect and one side sees "newer version" | The deploy changed the protocol version (`PROTOCOL_VERSION`): tabs opened before it only link with each other. The banner's **Reload** fixes it. Deploys that don't change the protocol never split a room. |
| Camera or microphone is blocked | The site must be served over HTTPS (both hosts do this; enable **Enforce HTTPS** for a GitHub custom domain). |
| Members show `via <name>` and can't hear each other | No direct path between them. Use the Cloudflare setup or add `&turn=…` (section 2). |
| Stuck at "Connecting to Relay" | The room's relays are unreachable or rate-limiting. With public relays: retry, or create rooms with your own relay (section 4). With your own relay: check `curl -H "Accept: application/nostr+json" https://relay.example.com/` and that the site's origin is in `--allowed-origin`. |
| Create Room stays disabled with "My relay" | The address must be `wss://…` (comma-separate several). `ws://` only works when the dchat page itself is plain `http://`. |
