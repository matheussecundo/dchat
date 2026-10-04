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

5. **Group Mesh Invariants (Phase 8)**:
   - **Topology**: full mesh, one `RTCPeerConnection` (`mesh::PeerLink`) per member pair. No SFU or any other server ever handles messages or media.
   - **Addressed signaling**: `Offer`/`Answer`/`IceBatch` carry the recipient session pubkey in `to` (inside the encrypted payload); `NostrRelayPool` drops signals addressed to someone else. The lower pubkey of each pair dials; perfect negotiation (polite = higher pubkey) handles any later glare.
   - **Signed room messages**: every data-channel message is a `RoomEnvelope` signed by its author's session key (`sign_message`, domain-separated from Nostr event signatures) and encrypted with the room key. Receivers verify the signature **before** recording the message id for dedup, then relay it to neighbors without a direct link to the author (`Roster::forward_targets`). Attribution always comes from the verified `author`, never from a self-declared name.
   - **Admin secret**: `admsk` exists only in the creator's admin link. The invite link, the QR code and **🔗 Copy Link** always use `invite_url()`, which strips it.
   - **Deterministic caps**: every member evaluates `Roster::evicted` / `latest_beyond_cap` on the same data; a member who finds itself evicted (room, voice seat or video slot) backs off on its own. Caps are enforced by honest clients, not cryptographically.
   - **Rekey**: only admin-signed `AdminRekey` envelopes are honored; grants are ECDH-sealed per recipient session key. Members offline during a rekey are stranded and need a fresh invite.
   - **History**: off unless `&hist=1`; only messages whose author's link had `hist=1` (`shareable`) are ever served, as signed originals verified by the receiver. RAM only; enforced by honest clients.
   - **Lounge media**: media flows only between members who both hold a voice seat over an open direct link (`sync_media_for`). Each link has at most one audio and one video `RtcRtpSender`; toggles use `replaceTrack`, never add/remove, so SDP does not grow.

6. **Hosting Anywhere (static, any path)**:
   - The production build must work at a domain root, under a path (`https://<user>.github.io/<repo>/`) and on IPFS: asset URLs are relative (`crates/client/Trunk.toml`), and code never navigates to `/`; use `state::page_base_url()` for "home".
   - The Service Worker only precaches files with fixed names (`./`, `./index.html`); hashed assets are cached on first fetch. Precaching a missing file makes its install fail on real hosts (the local dev server masks this by answering unknown paths with `index.html`).
   - `Cargo.lock` is committed so CI deploys the dependency versions that were tested (`--locked`).

7. **NAT Traversal (known limitation, must stay documented)**:
   - ICE uses STUN only by default (`stun:stun.l.google.com:19302`). Pairs behind carrier-grade NAT (mobile data) or symmetric NAT often cannot link directly: roughly 10–20% of pairs, and more pairs fail as a room grows.
   - Such pairs show `via <name>` in the member list: **text** still flows, gossip-relayed through a mutual member (signed, room-key encrypted). **Audio, video and files** need a direct link and are unavailable for that pair. With no mutual member the person stays `connecting…`.
   - The fix is an optional TURN server supplied in the URL fragment (`&turn=…&turnuser=…&turnpass=…`, see `build_rtc_config` in `session.rs`). TURN relays encrypted packets only; it sees IPs and timing, never content.
   - The README section "When Members Can't Connect Directly (NAT)" is the user-facing version; keep both in sync.

---

## 2. Repository Layout

```
dchat/
├── Cargo.toml                  # Workspace root (protocol, server, client)
├── crates/
│   ├── protocol/               # Shared types, messages, ChaCha20-Poly1305 crypto
│   │   └── src/
│   │       ├── crypto.rs       # 256-bit keygen, encrypt/decrypt, base64 helpers
│   │       ├── fragment.rs     # Order-preserving URL fragment parser (keeps unknown params)
│   │       ├── messages.rs     # Addressed SignalPayload, signed RoomEnvelope/RoomBody, ICE types
│   │       ├── nostr.rs        # NIP-01/16 types, BIP-340 Schnorr keys (k256), message signing, topic hashing
│   │       ├── room.rs         # Roster, link graph, gossip routing, member/voice/video cap eviction, RoomParams (pure, unit-tested)
│   │       └── lib.rs
│   ├── server/                 # Axum backend: Static file server + dev TLS + local mock Nostr relay
│   │   └── src/
│   │       ├── main.rs         # CLI args, LAN IP detection, QR code banner, /nostr & /ws routing
│   │       ├── nostr_relay.rs  # In-memory mock Nostr relay for local dev & offline E2E tests
│   │       ├── signaling.rs    # Legacy room manager
│   │       └── tls.rs          # rcgen self-signed dev certificate generator
│   └── client/                 # Leptos CSR frontend targeting wasm32-unknown-unknown
│       ├── Trunk.toml          # public_url = "./": relative asset paths (works under any path)
│       ├── index.html          # Trunk entry point & Service Worker registration
│       ├── style.css           # Responsive mobile-friendly dark theme
│       ├── service-worker.js   # Caches immutable static assets ONLY (never state)
│       ├── translations.json   # Embedded UI translation table for top 10 global languages
│       └── src/
│           ├── main.rs         # Leptos UI: create/join lobby, room view, member panel, modals
│           ├── i18n.rs         # Strongly typed i18n, browser detection, RTL handling
│           ├── media.rs        # Capture (mic/camera/screen), per-member <audio>, video attach, speaking meter
│           ├── mesh.rs         # PeerLink: one RTCPeerConnection per member (chat + file-transfer channels), perfect negotiation, batched ICE, tracks
│           ├── names.rs        # Random session names, name sanitizing, pubkey tags
│           ├── nostr_pool.rs   # Multi-relay pool, fan-out broadcast, deduplication, recipient filtering
│           ├── qr.rs           # On-the-fly SVG QR code generation
│           ├── session/
│           │   ├── mod.rs      # RoomSession: mesh orchestration, signed gossip, roster, caps, e2e hooks
│           │   ├── admin.rs    # Kick / rotate link: ECDH-sealed AdminRekey, migration to the new room
│           │   ├── extras.rs   # Typing, reactions, edit/delete, ECDH-sealed DMs, @mention detection
│           │   ├── files.rs    # Room-wide file cards, per-requester pulls, upload queue, chunk I/O
│           │   ├── history.rs  # Opt-in history (&hist=1): signed shareable messages for late joiners
│           │   └── lounge.rs   # Voice lounge: seats, per-link senders, voice/video caps, speaking, controls
│           └── state.rs        # UI types, URL fragment helpers (create room, invite/admin links), relays
├── e2e/                        # Playwright automated multi-peer end-to-end tests
│   ├── playwright.config.js    # Automatic server launch (serves crates/client/dist-e2e) and browser runner
│   └── tests/
│       ├── helpers.js          # createRoom / joinRoom / memberRow helpers shared by specs
│       ├── group_chat.spec.js  # 3-member mesh, fan-out, caps + admin seat, relayed text without a direct link
│       ├── group_voice.spec.js # 3-member lounge: join prompt, mesh audio, mute state, speaking, voice/video caps
│       ├── group_admin.spec.js # Kick (removed screen, others migrate with chat), rotate link (voice rejoin, old invite stranded)
│       ├── group_history.spec.js # hist=1 backlog incl. departed authors' names, default off, per-author shareable flag
│       ├── group_extras.spec.js  # Typing, reactions, edit/delete, direct + relayed DMs (relay can't read), @mention highlight
│       ├── p2p_chat.spec.js    # 2-member room, E2EE message exchange, reload wipe, fragment params
│       ├── audio_call.spec.js  # 2-member lounge audio, mic/speaker mute, leave (replaceTrack null) and rejoin
│       ├── video_call.spec.js  # Camera tiles, camera flip keeps the mic, camera off, grid teardown
│       ├── screen_share.spec.js# Screen share, switch to camera on the same sender, stop
│       ├── file_sharing.spec.js# Room-wide file cards: parallel pulls, decline/withdraw, upload queue, unreachable sender, sender leaving
│       ├── audio_settings.spec.js # Mic processing checkboxes, live track swap in voice, carry-over, reload reset
│       └── i18n.spec.js        # UI localization, dynamic switching, Arabic RTL, zero persistence
├── .github/workflows/pages.yml # GitHub Pages: unit tests, release build, no-hooks check, deploy
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
- **Phase 8: Discord-like Group Rooms over a Serverless Mesh (Completed)**
  - **8a Mesh core (Completed)**: create/join lobby with session nicknames; full-mesh `PeerLink`s with addressed signaling and perfect negotiation; signed `RoomEnvelope` gossip with relay to members lacking a direct link; roster with mutual-link reachability and `direct` / `via X` / `connecting` link states; per-room member cap (`&max=`, default 25, `0` = unlimited) with deterministic latest-joiner eviction and an admin seat; admin link (`adm`/`admsk`) vs invite link; optional TURN in the fragment; join/leave notices.
  - **8b Voice lounge (Completed)**: drop-in lounge replaces the ring flow (`CallInvite`/`CallAccepted` gone); signed `VoiceState` gossip (seat time, mic, video kind); per-room voice/video caps (`&maxa=`, `&maxv=`) with the same latest-loses rule (admins not exempt); one audio + one video sender per link (`addTrack` once, then `replaceTrack`, `None` to stop: no renegotiation on toggles); camera/screen as one video source with front/rear flip; per-member hidden `<audio class="remote-audio">`; Web Audio speaking meter; video grid with fullscreen; "X joined voice" prompt; audio settings carry over between joins.
  - **8c Group file sharing (Completed)**: `FileOffer` card gossiped to the room; `FileRequest`/`FileQueued`/`FileCancel{to}` are direct-only envelopes (`RoomBody::recipient`), applied only when received straight from their author and never relayed; per-link binary `file-transfer` channel with backpressure; chunks accepted only from the offer's author over its direct link and in order; `UploadQueue` (max 2 concurrent, FIFO, unit-tested); decline (local), cancel, withdraw (room-wide); transfers stop on link loss, offers marked unavailable when the author leaves the present set. Merged to main after 8c.
  - **8d Moderation & history (Completed)**: `AdminRekey { kicked, grants }` is gossiped and signed by an admin session (verified via its Hello admin proof); each grant (new room ID + key) is sealed with ECDH between the admin's and the member's session keys (`NostrBurnerKey::shared_key`, `SealedGrant`), so relays and the kicked member can't read it; recipients wait 1.5 s (relay flush), leave, rewrite the fragment and start a new session (chat kept, voice auto-rejoined, join notices muted for 5 s); the kicked member gets a removed screen. History: `Chat.shareable` from the author's own link, `HistoryBuffer` (200, signed originals) served to up to 2 neighbors on request in batches of 40 together with the authors' archived Hellos (names only, never roster); inserted by timestamp. Links stuck in `disconnected` for 10 s now count as lost so killed tabs leave promptly.
  - **8e Chat extras (Completed)**: `Typing` (throttled 3 s, expires after 4.5 s); `Reaction{target, emoji, on}` limited to `REACTIONS`, latest toggle per member wins (`Reactions`, unit-tested); `Edit`/`Delete` honored only from the original author (`message_authors`), and they drop the message from history; `Dm{to, sealed}` sealed with `seal_json` (ECDH session keys), sent only over the direct link when there is one, otherwise gossiped; only the recipient opens it and the recipient never relays it; `mentions()` with word boundaries on both sides; chime via Web Audio; `(n)` title badge while hidden; DM threads end when the peer leaves.

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

Also check the test-hook build compiles cleanly:
```bash
cargo check -p client --target wasm32-unknown-unknown --features e2e-hooks
```

### Step 3: Trunk Frontend Build
Rebuild the production bundle and the E2E bundle (with `e2e-hooks`, never deployed):
```bash
make build-client build-client-e2e
```
*Expected: `crates/client/dist/` and `crates/client/dist-e2e/` populated with `client-..._bg.wasm`, `client-...js`, and assets. `grep -c __dchat crates/client/dist/*.wasm` must print `0`.*

### Step 4: Playwright End-to-End Tests (Multi-Peer Simulation)
Run the headless multi-browser suite: it spins up the server on `dist-e2e`, builds 2–4 member meshes, negotiates WebRTC, exchanges signed encrypted messages, checks caps, relayed text and zero storage, and confirms memory wipe on reload:
```bash
cd e2e && npm test && cd ..
```
*Expected: all specs green (skips only for flows not yet rewritten in the current Phase 8 milestone). The suite takes about 15–30 seconds; mesh specs each allow up to 90 seconds.*

---

## 5. Security & Privacy Review Checklist

When writing or reviewing code, check off every item:
- [ ] No imports or usages of browser persistent storage (`localStorage`, `sessionStorage`, `indexedDB`, `cookies`).
- [ ] No logging of sensitive plaintext messages or cryptographic keys to console or server terminal.
- [ ] URL hash fragment keys are never included in HTTP query parameters or WebSocket URLs.
- [ ] The signaling server remains blind to message contents (payloads typed as `EncryptedPayload`).
- [ ] Service Worker cache strictly limits itself to immutable static assets (`.wasm`, `.js`, `.css`, `.html`).
- [ ] Room messages are `RoomEnvelope`s: signature verified before dedup/apply, attribution taken from the verified author only.
- [ ] The admin secret (`admsk`) never appears in the invite link, the QR code, logs, or any message.
- [ ] Untrusted input (names, signatures, SDP, envelopes from peers) is length-checked and never panics the client (k256 signature parsing panics on short input: use `parse_signature`).
- [ ] `e2e-hooks` code (`window.__dchat.selfPubkey/blockPeer/throttleUploads`) stays behind `#[cfg(feature = "e2e-hooks")]` and out of `make build-client` output.
- [ ] Direct-only room messages (`RoomBody::recipient()` is `Some`) are applied only when `to` is us and the envelope came straight from its author; they are never relayed.
- [ ] File chunks are accepted only from the offer's author over that author's own link, strictly in order; anything else aborts or is dropped.
- [ ] `AdminRekey` is honored only from a member whose Hello carried a valid admin proof; grants are sealed per recipient (never the room key in clear).
- [ ] DM plaintext is only ever sealed with `seal_json` to the recipient's session key; the recipient never relays a DM; no DM text in logs.
- [ ] Edits/deletes are applied only when the envelope author equals the original message author.
- [ ] History serves only `shareable` chat envelopes (author's choice) and the receiver verifies every signature; Hellos from history only label names, never join the roster.
