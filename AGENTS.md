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

3. **Dual-Layer End-to-End Encryption & Decentralized Signaling**:
   - **WebRTC Transport**: DTLS encryption for `RTCDataChannel` (text) and DTLS-SRTP for audio/video media streams.
   - **Application-Layer**: All signaling payloads (SDP offers/answers, ICE candidates) and `RTCDataChannel` messages are encrypted using **ChaCha20-Poly1305** with unique 96-bit nonces.
   - **Decentralized Nostr Relay Fabric**: Signaling is relayed over decentralized Nostr relays using **NIP-16 Ephemeral Events (Kind 20001)** signed in RAM by disposable BIP-340 Schnorr burner keys (`k256`). Room topics are hashed via SHA-256 (`["d", sha256(room_id)]`), ensuring relays and eavesdroppers remain 100% blind to room IDs, encryption keys, and message contents with zero disk persistence.

4. **Secure Context & Mobile WebRTC**:
   - Mobile browsers (Safari on iOS, Chrome on Android) require a Secure Context (`https://` or `localhost`) to access WebRTC and media devices.
   - The Axum server auto-generates self-signed dev TLS certificates via `rcgen` and outputs an ASCII QR code in the terminal for instant local network mobile pairing.
   - **Local Network Firewall**: If incoming connections from external devices on the LAN time out due to Linux firewall policies (e.g. UFW), open port 8443 ephemerally in RAM (cleared on reboot):
     ```bash
     sudo iptables -I INPUT -p tcp --dport 8443 -j ACCEPT
     ```
     Or persistently with UFW: `sudo ufw allow 8443/tcp` (`sudo ufw delete allow 8443/tcp` to remove).

---

## 2. Repository Layout

```
dchat/
├── Cargo.toml                  # Workspace root (protocol, server, client)
├── crates/
│   ├── protocol/               # Shared types, messages, ChaCha20-Poly1305 crypto
│   │   └── src/
│   │       ├── crypto.rs       # 256-bit keygen, encrypt/decrypt, base64 helpers
│   │       ├── messages.rs     # SignalPayload, DataChannelMessage, ICE types
│   │       ├── nostr.rs        # NIP-01/16 types, BIP-340 Schnorr burner keys (k256), topic hashing
│   │       └── lib.rs
│   ├── server/                 # Axum backend: Static file server + dev TLS + local mock Nostr relay
│   │   └── src/
│   │       ├── main.rs         # CLI args, LAN IP detection, QR code banner, /nostr & /ws routing
│   │       ├── nostr_relay.rs  # In-memory mock Nostr relay for local dev & offline E2E tests
│   │       ├── signaling.rs    # Legacy room manager
│   │       └── tls.rs          # rcgen self-signed dev certificate generator
│   └── client/                 # Leptos CSR frontend targeting wasm32-unknown-unknown
│       ├── index.html          # Trunk entry point & Service Worker registration
│       ├── style.css           # Responsive mobile-friendly dark theme
│       ├── service-worker.js   # Caches immutable static assets ONLY (never state)
│       ├── translations.json   # Embedded UI translation table for top 10 global languages
│       └── src/
│           ├── main.rs         # Leptos reactive UI, QR modal, Nostr relay modal, chat view
│           ├── i18n.rs         # Strongly typed i18n, browser detection, RTL handling
│           ├── nostr_pool.rs   # Multi-relay pool, fan-out broadcast, deduplication, burner key
│           ├── qr.rs           # On-the-fly SVG QR code generation
│           ├── state.rs        # Reactive connection state, URL hash & relay parser
│           └── webrtc.rs       # RtcPeerConnection & RTCDataChannel lifecycle with debounced ICE
├── e2e/                        # Playwright automated 2-peer end-to-end tests
│   ├── playwright.config.js    # Automatic server launch and browser runner
│   └── tests/
│       ├── p2p_chat.spec.js    # 2-peer handshake, E2EE message exchange, reload wipe
│       ├── audio_call.spec.js  # 2-peer audio call handshake, mute toggle, and end call
│       ├── video_call.spec.js  # 2-peer video call handshake, camera controls, and termination
│       ├── file_sharing.spec.js# P2P encrypted file sharing with multi-chunk transfer
│       ├── audio_settings.spec.js # Mic processing checkboxes, live track swap, reload reset
│       └── i18n.spec.js        # UI localization, dynamic switching, Arabic RTL, zero persistence
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

- **Phase 3: Video Calls & Screen Sharing (Completed)**
  - Camera capture with front/back camera flip toggle on mobile.
  - Screen capture via `web_sys::MediaDevices::get_display_media`.
  - Video stream rendering elements with fullscreen support.

- **Phase 4: Peer-to-Peer Encrypted File Sharing (Completed)**
  - Dedicated binary `RTCDataChannel` (`"file-transfer"`) with 64 KB chunking and backpressure control.
  - Per-chunk ChaCha20-Poly1305 AEAD authenticated encryption with header authentication (AAD).
  - Direct disk streaming via File System Access API (`showSaveFilePicker`) with fallback to in-memory Blob download.
  - Staging attachment chip, interactive file cards in chat, progress bars, real-time speed calculation, and decline/cancel controls.
  - Strict zero-persistence invariant: transfers abort and memory vanishes on peer disconnect or reload.

- **Phase 5: UI Localization & Multi-Language Support (Completed)**
  - Standalone translation table (`translations.json`) embedded directly into Wasm via `include_str!`.
  - Full support for the top 10 global languages: English (`en`), Chinese Simplified (`zh`), Hindi (`hi`), Spanish (`es`), French (`fr`), Arabic (`ar`), Bengali (`bn`), Portuguese (`pt`), Russian (`ru`), German (`de`).
  - Automatic browser localization detection on startup (`navigator.languages` / `navigator.language`) with English fallback.
  - Header `<select>` language switcher with native language labels.
  - Dynamic Right-to-Left (RTL) layout switching (`dir="rtl"`) for Arabic.
  - Strongly typed Rust accessor module (`i18n.rs`) with interpolation helpers.
- **Phase 6: Decentralized Nostr WebRTC Signaling (Completed)**
  - Replaced exclusive server requirement with decentralized Nostr relays.
  - Pure-Rust BIP-340 Schnorr burner keys (`k256`) and NIP-01/16 serialization compiled cleanly to `wasm32-unknown-unknown` with zero C dependencies.
  - Ephemeral Events (Kind 20001) ensuring compliant Nostr relays drop signaling packets immediately without disk storage.
  - Zero-knowledge payload encryption via ChaCha20-Poly1305 with SHA-256 room topic hashing (`["d", sha256(room_id)]`).
  - Concurrent multi-relay pool (`wss://relay.damus.io`, `wss://nos.lol`, `wss://relay.primal.net`) with fan-out publishing, automatic event deduplication, and URL hash/UI custom relay overrides.
  - Deterministic peer role coordination via lexicographical pubkey tie-breaker and WebRTC perfect negotiation.
  - Batched ICE candidate gathering with 100ms debouncing to prevent relay rate limiting.
  - In-memory mock Nostr relay in Axum server (`/nostr`) enabling 100% offline, deterministic automated Playwright tests.
  - Full serverless static hosting readiness (can deploy on GitHub Pages, Cloudflare, IPFS) while retaining Axum for local dev TLS and terminal QR printing.
- **Phase 7: Audio Processing Controls & Speaker Mute (Completed)**
  - ⚙️ Audio settings modal: noise cancellation, echo cancellation and auto gain control checkboxes (all on by default), requested as `getUserMedia` audio constraints built in one place (`build_audio_constraints` in `webrtc.rs`), the hook point for a future in-app denoiser such as RNNoise.
  - Mid-call changes re-capture the mic and `replaceTrack` it into the audio sender (latest-wins when changed rapidly), preserving mic mute state.
  - Unsupported switches (per `getSupportedConstraints()`) are disabled with a hint.
  - Local speaker mute on the hidden `#remote-audio` element for all call types; resets on call end. The peer is not notified.
  - Settings are RAM-only signals: they survive across calls within a tab and reset on reload.

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
