# AGENTS.md — AI Development & Verification Guide for dchat

`dchat` is an ephemeral, zero-knowledge, peer-to-peer WebRTC chat application written in pure Rust (compiled to `wasm32-unknown-unknown` without Emscripten). The application stores no messages on disk or persistent browser storage, relying solely on WebAssembly linear memory that vanishes on tab reload.

All automated and AI-driven development in this repository must adhere to the invariants, architecture rules, and testing requirements documented here.

---

## 1. Core Architectural Invariants

Every agent modifying this codebase must enforce these non-negotiable security and privacy invariants:

1. **Zero Persistence Invariant**:
   - **Forbidden APIs**: `localStorage`, `sessionStorage`, `indexedDB`, document `cookie`, or server-side disk databases.
   - **Rule**: All session state, encryption keys, and chat messages must reside strictly in Rust struct fields and reactive signals within WebAssembly linear memory.
   - **Verification**: Reloading the page or closing the tab must completely wipe all message history. Automated E2E tests check that `localStorage.length === 0` and `sessionStorage.length === 0`.

2. **Zero-Knowledge Key Distribution**:
   - **URL Hash Confidentiality**: The room ID and 256-bit symmetric encryption key are stored in the URL fragment (`#room=<id>&key=<base64_secret>`).
   - **Rule**: URL hash fragments are never sent to the server over HTTP requests or WebSocket handshakes. The server is completely blind to the decryption key.

3. **Dual-Layer End-to-End Encryption**:
   - **WebRTC Transport**: DTLS encryption for `RTCDataChannel` (text) and DTLS-SRTP for audio/video media streams.
   - **Application-Layer**: All signaling payloads (SDP offers/answers, ICE candidates) and `RTCDataChannel` messages are encrypted using **ChaCha20-Poly1305** with unique 96-bit nonces.
   - **Dumb Relay**: The Axum signaling server merely routes opaque `EncryptedPayload` envelopes between the two peers in a room.

4. **Secure Context & Mobile WebRTC**:
   - Mobile browsers (Safari on iOS, Chrome on Android) require a Secure Context (`https://` or `localhost`) to access WebRTC and media devices.
   - The Axum server auto-generates self-signed dev TLS certificates via `rcgen` and outputs an ASCII QR code in the terminal for instant local network mobile pairing.

---

## 2. Repository Layout

```
dchat/
├── Cargo.toml                  # Workspace root (protocol, server, client)
├── crates/
│   ├── protocol/               # Shared types, messages, ChaCha20-Poly1305 crypto
│   │   └── src/
│   │       ├── crypto.rs       # 256-bit keygen, encrypt/decrypt, base64 helpers
│   │       ├── messages.rs     # ClientMessage, ServerMessage, SignalPayload, DataChannelMessage
│   │       └── lib.rs
│   ├── server/                 # Axum backend: WebSocket relay + static file server
│   │   └── src/
│   │       ├── main.rs         # CLI args, LAN IP detection, QR code banner
│   │       ├── signaling.rs    # In-memory room manager (max 2 peers, zero persistence)
│   │       └── tls.rs          # rcgen self-signed dev certificate generator
│   └── client/                 # Leptos CSR frontend targeting wasm32-unknown-unknown
│       ├── index.html          # Trunk entry point & Service Worker registration
│       ├── style.css           # Responsive mobile-friendly dark theme
│       ├── service-worker.js   # Caches immutable static assets ONLY (never state)
│       └── src/
│           ├── main.rs         # Leptos reactive UI, QR modal, chat view
│           ├── qr.rs           # On-the-fly SVG QR code generation
│           ├── state.rs        # Reactive connection state, URL hash parser
│           └── webrtc.rs       # RtcPeerConnection & RTCDataChannel lifecycle
├── e2e/                        # Playwright automated 2-peer end-to-end tests
│   ├── playwright.config.js    # Automatic server launch and browser runner
│   └── tests/
│       └── p2p_chat.spec.js    # 2-peer handshake, E2EE message exchange, reload wipe
├── README.md                   # User guide, building, running locally, mobile test
└── AGENTS.md                   # This document
```

---

## 3. Phased Roadmap

- **Phase 1: Ephemeral Encrypted Text Chat (Completed)**
  - Pure Rust Leptos CSR client built with Trunk.
  - ChaCha20-Poly1305 encryption on `RTCDataChannel` and signaling envelopes.
  - Zero-knowledge URL fragment key handling (`#room=...&key=...`).
  - Terminal QR code + in-app SVG QR code for instant phone scanning.
  - Axum server with auto-generated dev TLS and static file serving.
  - Static asset-only Service Worker caching.
  - Multi-tiered automated tests (Cargo unit/integration + Playwright 2-peer E2E).

- **Phase 2: Audio Calls (Completed)**
  - Capture microphone via `web_sys::MediaDevices::get_user_media_with_constraints`.
  - Add audio tracks to `RtcPeerConnection`.
  - In-app microphone mute/unmute UI controls.
  - Handle audio stream reception and playback via HTML `<audio>` elements.

- **Phase 3: Video Calls Phase 3: Video Calls & Screen Sharing Screen Sharing (Next Sprint)**
  - Camera capture with front/back camera flip toggle on mobile.
  - Screen capture via `web_sys::MediaDevices::get_display_media`.
  - Video stream rendering elements with fullscreen support.

---

## 4. Verification Loop for AI Agents

Whenever making changes, an agent must execute the following test matrix before marking work as complete:

### Step 1: Protocol & Server Unit Tests
Verify cryptographic primitives, serialization, and room lifecycle in memory:
```bash
cargo test --workspace
```
*Expected: 100% pass, 0 failures, 0 ignored.*

### Step 2: Client WebAssembly Type Checking
Verify client compilation for the `wasm32-unknown-unknown` target:
```bash
cargo check -p client --target wasm32-unknown-unknown
```
*Expected: 0 errors, 0 warnings.*

### Step 3: Trunk Frontend Build
Rebuild the static distribution bundle:
```bash
cd crates/client && trunk build index.html && cd ../..
```
*Expected: `dist/` directory populated with `client-..._bg.wasm`, `client-...js`, and assets.*

### Step 4: Playwright End-to-End Test (2-Peer Simulation)
Run the headless multi-browser test that spins up the server, connects 2 peers, negotiates WebRTC, sends encrypted messages, verifies zero storage, and confirms memory wipe on reload:
```bash
cd e2e && npm test && cd ..
```
*Expected: Test passes in under 5 seconds with green status.*

---

## 5. Security & Privacy Review Checklist

When writing or reviewing code, check off every item:
- [ ] No imports or usages of browser persistent storage (`localStorage`, `sessionStorage`, `indexedDB`, `cookies`).
- [ ] No logging of sensitive plaintext messages or cryptographic keys to console or server terminal.
- [ ] URL hash fragment keys are never included in HTTP query parameters or WebSocket URLs.
- [ ] The signaling server remains blind to message contents (payloads typed as `EncryptedPayload`).
- [ ] Service Worker cache strictly limits itself to immutable static assets (`.wasm`, `.js`, `.css`, `.html`).
