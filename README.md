# 🔒 dchat — Ephemeral Zero-Knowledge P2P Chat

> **Disposable, browser-cached peer-to-peer chat built in pure Rust and WebAssembly, bypassing Emscripten entirely.**

All chat state, keys, and message history reside strictly in WebAssembly linear memory. If a user refreshes or closes the page, the session vanishes forever.

---

## Key Features & Security Guarantees

* **Pure Rust to WebAssembly**: Built with **Leptos (CSR)** targeting `wasm32-unknown-unknown` without Emscripten.
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
`dchat` needs **no backend server** in production. Signaling occurs over decentralized Nostr relays, and WebAssembly executes directly in the browser:
```bash
cd crates/client
trunk build --release index.html
cd ../..
```
Deploy the resulting `crates/client/dist/` directory directly to any static host:
- **GitHub Pages / Cloudflare Pages / Vercel / Netlify**
- **IPFS / Arweave**
- Any static file server (`caddy`, `nginx`, `python3 -m http.server`)

*(Note: WebRTC requires HTTPS when not served from `localhost`.)*

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
4. Phone 1 will load the app and auto-generate an ephemeral room and encryption key in the URL hash (e.g. `#room=9x2f4b&key=...`).

### Step 3: Connect Phone 2
1. On Phone 1's screen, tap the **"📱 Scan QR"** button at the top.
2. A modal will appear with a QR code encoding Phone 1's exact room URL and encryption key.
3. Open the camera on Phone 2 and scan Phone 1's screen.
4. Phone 2 opens the room. The WebRTC handshake will complete in milliseconds, and both status badges will change to **"Connected (E2EE P2P Active)"** with a green indicator!

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

### 2. Playwright Multi-Browser End-to-End (E2E) Test
Simulates two separate browser instances performing the full WebRTC handshake, sending encrypted messages, verifying that `localStorage` and `sessionStorage` are completely empty, and confirming memory wipe on page reload:
```bash
cd e2e
npm test
```

---

## Project Governance

For AI agents modifying or extending this codebase, refer to [`AGENTS.md`](./AGENTS.md) for architectural invariants, testing checklists, and verification procedures.
