# 🔒 dchat — Ephemeral Zero-Knowledge P2P Chat

> **Disposable, browser-cached peer-to-peer chat built in pure Rust and WebAssembly, bypassing Emscripten entirely.**

All chat state, keys, and message history reside strictly in WebAssembly linear memory. If a user refreshes or closes the page, the session vanishes forever.

---

## Key Features & Security Guarantees

* **Pure Rust to WebAssembly**: Built with **Leptos (CSR)** targeting `wasm32-unknown-unknown` without Emscripten.
* **Zero Persistence**: Strictly **no** `localStorage`, `sessionStorage`, `indexedDB`, or cookies. Everything is held in volatile RAM.
* **Zero-Knowledge URL Keys**: The room ID and 256-bit symmetric encryption key reside in the URL fragment (`#room=<id>&key=<secret>`). URL hash fragments are never sent to the server over HTTP or WebSocket handshakes.
* **Dual-Layer E2EE**: In addition to standard WebRTC DTLS, messages and signaling envelopes are encrypted with **ChaCha20-Poly1305** using the URL fragment key. The signaling relay is completely blind to message contents.
* **Instant Destruction**: Reloading the page or closing the tab wipes linear memory, destroys the WebRTC connection, and permanently erases all history.
* **Service Worker Caching**: Caches immutable application shell assets (`.wasm`, `.js`, `.css`) for instant loading, while never storing session or user data.
* **Instant Mobile Testing**: Built-in dev HTTPS server auto-generates TLS certificates and displays an ASCII QR code in the terminal for instant phone pairing on local Wi-Fi.

---

## 3-Phase Roadmap

1. **Phase 1: Ephemeral Encrypted Text Chat (Current)**
   - P2P text chat via WebRTC `RTCDataChannel`.
   - ChaCha20-Poly1305 application-layer encryption.
   - URL hash key generation & QR code sharing modal.
   - Axum WebSocket signaling relay and dev TLS server.
   - Comprehensive unit and Playwright multi-browser E2E tests.

2. **Phase 2: Encrypted Audio Calls (Completed)**
   - Capture microphone audio via `getUserMedia`.
   - DTLS-SRTP encrypted peer audio streaming.
   - Mute/unmute microphone controls in the UI.

3. **Phase 3: Video Calls Phase 3: Video Calls & Screen Capture Screen Capture (Next Sprint)**
   - Camera capture with front/back camera toggling on mobile.
   - Screen capture via `getDisplayMedia`.
   - Fullscreen video rendering.

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

## Building the Project

### 1. Build the WebAssembly Frontend
```bash
cd crates/client
trunk build --release index.html
cd ../..
```
This compiles the Leptos app into `crates/client/dist/` containing optimized `.wasm`, JavaScript glue, styles, and the static Service Worker.

### 2. Build the Axum Server
```bash
cargo build -p server --release
```

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
