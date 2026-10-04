# 🔒 dchat — Ephemeral Zero-Knowledge P2P Chat

> **Disposable, browser-cached peer-to-peer chat built in pure Rust and WebAssembly, bypassing Emscripten entirely.**

All chat state, keys, and message history reside strictly in WebAssembly linear memory. If a user refreshes or closes the page, the session vanishes forever.

---

## Key Features & Security Guarantees

* **Pure Rust to WebAssembly**: Built with **Leptos (CSR)** targeting `wasm32-unknown-unknown` without Emscripten.
* **Group Rooms over a P2P Mesh**: One link opens a room for a small group (25 members by default). Every member connects directly to every other member: there is no media or message server, only Nostr relays for the initial handshake.
* **Zero Persistence**: Strictly **no** `localStorage`, `sessionStorage`, `indexedDB`, or cookies. Everything is held in volatile RAM.
* **Zero-Knowledge URL Keys**: The room ID and 256-bit symmetric encryption key reside in the URL fragment (`#room=<id>&key=<secret>`). URL hash fragments are never sent to any server over HTTP or WebSocket handshakes.
* **Decentralized Nostr Signaling**: Replaces proprietary signaling servers with public, open Nostr relays using NIP-16 Ephemeral Events (Kind 20001, dropped upon dispatch, zero disk storage).
* **Serverless Architecture**: 100% static client deployable to GitHub Pages, Cloudflare Pages, Netlify, or IPFS. No backend required!
* **Multi-Relay Pool Resilience**: Broadcasts and subscribes across a concurrent relay pool (`wss://relay.damus.io`, `wss://nos.lol`, `wss://relay.primal.net`) with automatic event deduplication.
* **Ephemeral Burner Keypairs**: Generates fresh in-memory secp256k1 burner keypairs per session for BIP-340 Schnorr event signatures, discarding them on exit.
* **Dual-Layer E2EE**: In addition to standard WebRTC DTLS, messages and signaling envelopes are encrypted with **ChaCha20-Poly1305** using the URL fragment key. Relays are completely blind to message contents.
* **Instant Destruction**: Reloading the page or closing the tab wipes linear memory, destroys the WebRTC connection, and permanently erases all history.
* **Service Worker Caching**: Caches immutable application shell assets (`.wasm`, `.js`, `.css`) for instant loading, while never storing session or user data.
* **Instant Mobile Testing**: Built-in dev HTTPS server auto-generates TLS certificates, includes an in-memory mock Nostr relay, and displays an ASCII QR code in the terminal for instant phone pairing on local Wi-Fi.

---

## Phased Roadmap

1. **Phase 1: Ephemeral Encrypted Text Chat (Completed)**
   - P2P text chat via WebRTC `RTCDataChannel`.
   - ChaCha20-Poly1305 application-layer encryption.
   - URL hash key generation & QR code sharing modal.
   - Axum WebSocket signaling relay and dev TLS server.
   - Comprehensive unit and Playwright multi-browser E2E tests.

2. **Phase 2: Encrypted Audio Calls (Completed)**
   - Capture microphone audio via `getUserMedia`.
   - DTLS-SRTP encrypted peer audio streaming.
   - Mute/unmute microphone controls in the UI.

3. **Phase 3: Video Calls & Screen Sharing (Completed)**
   - Camera capture with front/back camera toggling on mobile.
   - Screen capture via `getDisplayMedia`.
   - Fullscreen video rendering.

4. **Phase 4: Encrypted P2P File Sharing (Completed)**
   - Zero-knowledge end-to-end encrypted file sharing directly between peers.
   - Binary WebRTC data channel with 64 KB chunking and backpressure throttling.
   - Streaming disk write via File System Access API with automatic Blob download fallback.
   - Per-chunk ChaCha20-Poly1305 authenticated encryption with AEAD header authentication.
   - Interactive file cards in chat with real-time transfer progress, speed metrics, and cancel/decline controls.
   - Completely ephemeral: files are never stored on any server or persistent browser storage; downloads cease if sender disconnects.

5. **Phase 5: UI Localization & Multi-Language Support (Completed)**
   - Standalone `translations.json` table embedded directly into Wasm via `include_str!`.
   - 10 most common global languages: English (`en`), Chinese (`zh`), Hindi (`hi`), Spanish (`es`), French (`fr`), Arabic (`ar`), Bengali (`bn`), Portuguese (`pt`), Russian (`ru`), German (`de`).
   - Browser localization detection on startup with automatic English fallback.
   - Header dropdown language switcher with native labels.
   - Dynamic Right-to-Left (RTL) layout switching (`dir="rtl"`) for Arabic.
   - Zero Persistence Invariant: language choices reside purely in Wasm RAM.

6. **Phase 6: Decentralized Nostr WebRTC Signaling (Completed)**
   - Decentralized signaling via Nostr relay pool with NIP-16 Ephemeral Events (Kind 20001).
   - In-memory secp256k1 ephemeral burner keypairs (`k256`) with BIP-340 Schnorr signatures in pure Wasm.
   - SHA-256 room topic hashing (`["d", sha256(room_id)]`) to prevent leaking room IDs to relays.
   - Perfect negotiation and deterministic initiator/responder role assignment.
   - Batched ICE candidate exchange with 100ms debouncing to eliminate relay rate-limiting.
   - URL fragment relay configuration (`#room=...&key=...&relays=wss://...`).
   - Interactive Nostr relay badge and status modal in header.
   - In-memory mock Nostr relay in dev server for 100% offline, deterministic automated testing.

7. **Phase 7: Audio Processing Controls & Speaker Mute (Completed)**
   - ⚙️ Audio settings modal with three checkboxes, all on by default: noise cancellation, echo cancellation and auto gain control (browser-native `getUserMedia` constraints).
   - Changes apply live mid-call by re-capturing the mic and swapping the sent track (`replaceTrack`, no renegotiation); mic mute state is kept.
   - Switches the browser doesn't support are disabled with a hint.
   - Speaker mute silences incoming peer audio locally in audio, video and screen-share calls; it resets when the call ends.
   - Zero Persistence Invariant: settings live in Wasm RAM only and reset to all-on on reload.

8. **Phase 8: Discord-like Group Rooms over a Serverless Mesh (Completed)**
   - **8a Mesh core (Completed)**: one `RTCPeerConnection` per member pair with perfect negotiation; recipient-addressed signaling; session nicknames shown with a short key tag; live member list; room messages signed by their author and gossip-relayed to members without a direct link; per-room member cap with deterministic "latest joiner loses" and an admin seat; admin link vs invite link; optional TURN server in the URL.
   - **8b Voice lounge (Completed)**: Discord-style drop-in voice/video lounge replacing the 1:1 ring flow; per-person mic, speaker, camera (with front/rear flip) and screen-share toggles; video grid with fullscreen; speaking indicator; "X joined voice" prompt; per-room voice and video limits; audio processing settings carry over.
   - **8c Group file sharing (Completed)**: room-wide file cards; each member pulls the file straight from the sender over their own direct link; the sender uploads to at most 2 members at once and queues the rest; decline, cancel and withdraw; transfers stop if the sender leaves.
   - **8d Moderation & history (Completed)**: admins can kick a member or move everyone to a new link (the room ID and key change, sealed to each remaining member); opt-in history so late joiners see the last 200 messages.
   - **8e Chat extras (Completed)**: typing indicator, emoji reactions, editing and deleting your own messages, private DMs sealed end-to-end between two members, and @mentions with a highlight, a title badge and a chime.

---

## Group Rooms

1. **Create**: open the app, pick a name for this session and a member limit, then tap **✨ Create Room**. The room ID, the room key and an admin key are generated in your browser and placed in the URL fragment.
2. **Invite**: tap **🔗 Copy Link** or **📱 Scan QR**. Both share the *invite* link. Only the creator also sees **🔑 Copy Admin Link**, which adds the admin secret (`admsk`); share it only with co-moderators.
3. **Join**: whoever opens the invite picks a name and taps **🚪 Enter Room**. Names live in memory only and are shown with a 4-character key tag (`Ana · 3f2a`), so two people with the same name stay distinct. The tag tells people apart; it is not proof of identity.
4. **Member list**: each member shows how you reach them: `direct`, `via <name>` (no direct link, text is relayed through that member), or `connecting…`. The admin carries an `ADMIN` badge.
5. **Moderation (admin link only)**: next to each member, **Kick** moves everyone else to a new room ID and key; the kicked member sees *You were removed from the room*. **🔄 New Link** does the same without removing anyone, so the old invite stops working. Chat history on screen is kept and anyone in voice is reconnected automatically. Share the new invite (**🔗 Copy Link**) with anyone who was offline during the move: they can't follow on their own. Kicking needs an admin online, and admins can't kick each other.
6. **Member limit**: when a room is full, the member who joined last sees *Room is full*. Everyone applies the same rule (join time, then key), so all members agree on who stays. An admin session always gets a seat and bumps the latest non-admin.

Every room message is signed with its author's session key, so a member relaying it cannot alter it or forge messages from someone else.

**History for late joiners** is off by default: you only see messages sent while you are in the room. The creator can tick *Let late joiners see the last 200 messages*, which adds `&hist=1` to the link and shows a 🕒 badge. Members then keep recent messages in memory (never on disk) and hand them to newcomers as signed originals, so they can't be altered. Each message carries its author's own setting: someone who joined with a link without `hist=1` keeps their messages out of history. History disappears when the last member leaves. Like any chat, "off" can't stop someone who is present from copying a message.

### Chat Extras

- **Typing**: *"Bo is typing…"* appears above the message box.
- **Reactions**: hover a message and tap 😀 to add 👍 ❤️ 😂 😮 😢 🎉; tap a reaction chip to add or remove yours.
- **Edit / delete your own messages**: ✏️ puts the text back in the box (Enter saves, Esc cancels) and others see *(edited)*. 🗑️ removes it for everyone. Both are signed by you; deletion is best effort, since anyone may have already read or copied the message. Edited or deleted messages leave the history shown to late joiners.
- **Private messages**: ✉️ next to a member opens a private chat. Messages are sealed with a key only the two of you can derive (ECDH between your session keys). With a direct link they travel only over that link; otherwise other members relay them without being able to read them. The conversation ends when either of you leaves, because session keys are per tab.
- **@mentions**: write `@Name` and that member sees the message highlighted, hears a short chime and, if the tab is in the background, gets a `(n)` badge in the tab title.

### Voice Lounge

Each room has one drop-in voice lounge. Nobody is rung:
- Tap **🔊 Join Voice** to enter. Members outside voice see a short *"Ana joined voice"* prompt with a **Join** button.
- Inside, the controls are 🎙️ mic mute, 🔊 speaker mute (local only), 📹 camera (🔄 flips front/rear), 🖥️ screen share, ⚙️ audio processing and **📴 Leave**.
- Camera and screen share are one video source at a time; switching between them reuses the same connection.
- Members with video appear in a grid (⛶ for fullscreen). Whoever is talking gets a green ring, measured locally from the audio level.
- Audio and video only flow between members who are in the lounge, directly peer-to-peer (DTLS-SRTP). A member you only reach `via` someone else is shown with ⚠: you can't hear or see each other without a direct link (see NAT below).
- **Voice limit** (default 8) and **video limit** (default 6) are set when creating the room. When the lounge is full, **Join Voice** is disabled; if two people race for the last seat, the one who joined last is moved out, using the same rule as the member limit. The limits apply to admins too.

### Sharing Files

Tap **📎**, pick a file, optionally add a caption and send. Everyone in the room sees the card:
- Each member who taps **⬇️ Download** pulls the file **directly from the sender** over their own WebRTC link. Every 64 KB chunk is sealed with ChaCha20-Poly1305 using the room key. Files are never relayed through other members or any server.
- The sender uploads to at most **2 members at a time**; others see *⏳ Queued (#n)* until a slot frees up. The sender's card shows how many are sending, waiting and done.
- **Decline** just hides the buttons for you. The sender can **Withdraw** the offer for everyone, which also stops transfers in progress.
- Downloads stream to disk when the browser supports the File System Access API; otherwise they are assembled in memory (with a warning above 250 MB).
- If you have no direct link to the sender (`via` in the member list), the card says *Sender not directly reachable*. If the sender leaves, pending offers are marked unavailable and running transfers stop.

### URL Fragment Parameters

Everything after `#` stays in the browser and is never sent to any server.

| Parameter | Example | Meaning |
|---|---|---|
| `room` | `room=jr9m4r26` | Room ID (hashed before it reaches relays) |
| `key` | `key=Zm9v…` | 256-bit room key (base64url) |
| `adm` | `adm=9f3c…` | Admin public key; sessions proving it get the `ADMIN` badge and a guaranteed seat |
| `admsk` | `admsk=…` | Admin secret key: **admin link only**, never in the invite or QR code |
| `max` | `max=10` | Member limit; default 25, `0` = unlimited (no hard ceiling; large rooms load every member) |
| `maxa` | `maxa=4` | Voice limit: members in the lounge at once; default 8, `0` = unlimited |
| `maxv` | `maxv=2` | Video limit: cameras/screens on at once; default 6, `0` = unlimited |
| `hist` | `hist=1` | Late joiners may see the last 200 shareable messages (off when absent) |
| `relays` | `relays=wss://a,wss://b` | Custom Nostr relays |
| `turn` | `turn=turns:turn.example.com:5349` | Optional TURN server(s), comma-separated |
| `turnuser`, `turnpass` | `turnuser=me&turnpass=s3cret` | TURN credentials (percent-encode special characters) |

### When Members Can't Connect Directly (NAT)

dchat uses STUN only by default, so it needs no infrastructure of its own. Most home and office networks connect fine. But two members behind **carrier-grade NAT** (common on mobile data) or **symmetric NAT** often cannot open a direct WebRTC link: roughly 10–20% of pairs. In a group mesh, the more members a room has, the more likely it is that some pair fails.

What you will see:
- The member list shows the other person as **`via <name>`** instead of `direct`.
- **Text still works**: messages are relayed through a member who is connected to both of you. They stay encrypted with the room key and signed by their author, so the relaying member (who is in the room anyway) cannot alter or forge them.
- **Voice, video and files don't work with that person**: media and file transfers only travel over direct links (the lounge marks them with ⚠, file cards say *Sender not directly reachable*).
- If no mutual member exists, the person stays `connecting…` until a path appears.

**Fix: add a TURN server.** A TURN server forwards encrypted packets between members who can't reach each other. It sees IP addresses and traffic timing, but never message or media content (DTLS/SRTP plus the room key). There are two ways to provide one:

- **From the host (recommended):** when the site is deployed on Cloudflare (see "Cloudflare (Workers + TURN)" below), the app asks the site for fresh, short-lived Cloudflare TURN credentials each time you enter a room (`GET ./ice-servers`). Nothing needs to go in the link. On hosts without that endpoint the request just returns 404 and the app carries on with STUN.
- **In the room link:** add a TURN server you run yourself (for example [coturn](https://github.com/coturn/coturn)) and share that URL:

  ```
  https://your-domain.com/#room=…&key=…&turn=turns:turn.example.com:5349&turnuser=alice&turnpass=s3cret
  ```

  Everyone who opens the link uses it, and the credentials are visible to all members, so use credentials scoped to this purpose.

---

## Prerequisites

- [Rust](https://rustup.rs/) (edition 2021, rustc 1.80+)
- `wasm32-unknown-unknown` target:
  ```bash
  rustup target add wasm32-unknown-unknown
  ```
- [Trunk](https://trunkrs.dev/) (Wasm web application bundler):
  ```bash
  cargo install --locked trunk
  ```
- [Node.js](https://nodejs.org/) (v18+) for running the automated Playwright E2E tests.

---

## Building & Deploying

### 1. Serverless Static Deployment (Production)
> Step-by-step guide for GitHub Pages and Cloudflare (Workers + TURN), including custom domains, verification and troubleshooting: [`docs/DEPLOYMENT.md`](./docs/DEPLOYMENT.md).

`dchat` needs **no backend server** in production. Signaling occurs over decentralized Nostr relays, and WebAssembly executes directly in the browser:
```bash
cd crates/client
trunk build --release
cd ../..
```
Deploy the resulting `crates/client/dist/` directory to any static host. Asset paths are relative (`public_url = "./"` in `crates/client/Trunk.toml`), so the same build works at a domain root, under a path such as `https://<user>.github.io/<repo>/`, or on IPFS:
- **GitHub Pages / Cloudflare Pages / Vercel / Netlify**
- **IPFS / Arweave**
- Any static file server (`caddy`, `nginx`, `python3 -m http.server`)

*(Note: WebRTC requires HTTPS when not served from `localhost`.)*

#### GitHub Pages (automated)
`.github/workflows/pages.yml` runs the unit tests, builds the client, checks that no test hooks are in the bundle and publishes `dist/` on every push to `main`:
1. Push the repository to GitHub.
2. In the repository, open **Settings → Pages** and set **Source** to **GitHub Actions** (one time).
3. Push to `main` (or run the workflow by hand from the **Actions** tab). The site appears at `https://<user>.github.io/<repo>/`.

#### Cloudflare (Workers + TURN)
`wrangler.jsonc` deploys the client as a Cloudflare Worker with static assets, plus `worker/` with one endpoint, `GET /ice-servers`. It returns short-lived [Cloudflare Realtime TURN](https://developers.cloudflare.com/realtime/turn/) credentials (12-hour lifetime). The TURN API token stays in the Worker; the request carries no room information, because the room ID and key live in the URL fragment, which is never sent. The endpoint only answers same-origin browser requests and allows 20 requests per minute per IP, so other sites can't spend your quota.

One-time setup:
1. In the Cloudflare dashboard, open **Realtime → TURN** and create a TURN key. Note its **key ID** and **API token**.
2. Create an API token for deploys (template **Edit Cloudflare Workers**) and note your **account ID**.
3. In the GitHub repository, add the secrets `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID`. From then on, `.github/workflows/cloudflare.yml` deploys on every push to `main`; without these secrets it skips the deploy. If you use Cloudflare only, disable the GitHub Pages workflow in the **Actions** tab.
4. After the first deploy, give the Worker the TURN key (once; it is kept across deploys): `npx wrangler secret put TURN_KEY_ID` and `npx wrangler secret put TURN_KEY_API_TOKEN`, or add both under the Worker's **Settings → Variables and Secrets**.

The site is then at `https://dchat.<your-subdomain>.workers.dev` (or a custom domain). Without the TURN secrets the site still works, just without TURN.

To deploy from your machine instead: `npx wrangler login`, then `make deploy-cloudflare`. Wrangler 4 needs Node.js 22 or newer.

TURN pricing: $0.05 per GB the TURN server sends to clients, after a free tier of 1,000 GB. Only members who can't connect directly use it. A credential stops working after 12 hours, so a call longer than that loses its relay; rejoining the room fetches fresh credentials.

Once hosted, rooms use the public Nostr relays below instead of the local mock relay. For dependable voice and video across mobile networks, add TURN: deploy on Cloudflare (below), or add your own TURN server to room links (see "When Members Can't Connect Directly (NAT)").

### 2. Custom Nostr Relays
By default, `dchat` connects to a resilient pool of public Nostr relays:
- `wss://relay.damus.io`
- `wss://nos.lol`
- `wss://relay.primal.net`

You can customize which relays to use directly via the URL fragment parameter `relays`:
```
https://your-domain.com/#room=room123&key=SECRET_KEY&relays=wss://my-relay.org,wss://nostr.land
```
You can also view connected relays and their live status by clicking the **⚡ Nostr** badge in the header.

### 3. Local Development Runner (Optional Axum Dev Server)
For local development, testing without internet access, and mobile testing on local Wi-Fi:
```bash
cargo build -p server --release
```
The Axum server serves the static frontend, generates a self-signed TLS certificate with a terminal QR code, and provides an in-memory mock Nostr relay at `/nostr`.

---

## Testing Locally with 2 Cellphones

Mobile browsers (iOS Safari and Android Chrome) require a **Secure Context (`https://` or `localhost`)** to allow WebRTC permissions. `dchat` includes auto-generated TLS certificates and terminal QR codes specifically for seamless local mobile testing.

### Step 1: Start the Server
Run the unified Axum server:
```bash
cargo run -p server --release
```
The server will bind to `0.0.0.0:8443`, detect your machine's LAN IP, and render an ASCII QR code in your terminal:

```
====================================================================
  🔒 dchat - Ephemeral Zero-Knowledge P2P Chat Server
====================================================================
  Mode:         HTTPS (Self-Signed Dev TLS)
  Localhost:    https://localhost:8443
  Mobile LAN:   https://192.168.1.150:8443
--------------------------------------------------------------------
  Scan this QR Code with your cellphones to connect:
  [ ASCII QR CODE ]
====================================================================
```

> [!TIP]
> **Linux Firewall Note (Connection Timed Out)**:
> If incoming connections from another computer or phone time out, your Linux firewall may be dropping incoming packets on port 8443.
> - **Ephemeral / In-Memory (resets on reboot, zero persistence)**:
>   ```bash
>   sudo iptables -I INPUT -p tcp --dport 8443 -j ACCEPT
>   ```
> - **Persistent with UFW**:
>   ```bash
>   sudo ufw allow 8443/tcp
>   # To remove after testing:
>   sudo ufw delete allow 8443/tcp
>   ```

### Step 2: Connect Phone 1
1. Make sure your phone is connected to the **same Wi-Fi network** as your computer.
2. Open your phone's camera and scan the QR code printed in the terminal (or open `https://<YOUR-LAN-IP>:8443`).
3. **Accept the Dev Certificate**: Because the certificate is self-signed for local development, your browser will display a warning:
   - **Chrome (Android)**: Tap *Advanced* → *Proceed to 192.168.x.x (unsafe)*.
   - **Safari (iOS)**: Tap *Show Details* → *Visit this website* → Confirm.
4. Phone 1 loads the lobby: pick a name and tap **✨ Create Room**. An ephemeral room and encryption key are generated in the URL hash (e.g. `#room=9x2f4b&key=...`).

### Step 3: Connect Phone 2
1. On Phone 1's screen, tap the **"📱 Scan QR"** button at the top.
2. A modal will appear with a QR code encoding the room's invite link (room ID and key, without the admin secret).
3. Open the camera on Phone 2 and scan Phone 1's screen.
4. Phone 2 opens the room, picks a name and taps **🚪 Enter Room**. The WebRTC handshake completes in moments, both status badges change to **"Connected (E2EE P2P Active)"**, and each phone lists the other in the member list.
5. More phones or laptops can join the same way; every member connects directly to every other member.

### Step 4: Chat & Verify Ephemerality
- Type messages on either phone and watch them appear in real time over the direct encrypted `RTCDataChannel`.
- Tap **"💥 Wipe Session"** or refresh the browser: all message history is permanently destroyed from RAM.

---

## Automated Verification & Testing

The repository contains a multi-tiered test suite for automated CI/CD and AI-driven development:

### 1. Protocol & Crypto Unit Tests
Verifies 256-bit key generation, ChaCha20-Poly1305 encryption/decryption roundtrips, bad nonce/tamper rejection, and room state machine:
```bash
cargo test --workspace
```

### 2. Playwright Multi-Browser End-to-End (E2E) Tests
Simulates several isolated browser members: WebRTC mesh handshakes, signed message fan-out, member caps and the admin seat, text relayed between members without a direct link, empty `localStorage`/`sessionStorage`, and memory wipe on reload.

The E2E suite runs against a separate bundle built with the `e2e-hooks` feature (test-only `window.__dchat` probes, e.g. to simulate a pair that cannot connect). That bundle goes to `crates/client/dist-e2e/` and is never deployed; production builds contain no hooks.
```bash
make test-e2e
# or, step by step:
cd crates/client && trunk build index.html --release --features e2e-hooks --dist dist-e2e && cd ../..
cd e2e && npm test
```

---

## Project Governance

For AI agents modifying or extending this codebase, refer to [`AGENTS.md`](./AGENTS.md) for architectural invariants, testing checklists, and verification procedures.
