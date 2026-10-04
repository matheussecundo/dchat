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
   - **Room passwords** (`&pw=<salt>`, `protocol::password`): room key = SHA-256(link key, Argon2id(password, salt)), relay topic = `password_room_topic` (so the link alone doesn't find the room). The stretched value stays in RAM and re-derives the key after a rekey (grants carry link keys, never derived keys). Never put a password verifier in the link: it would allow offline guessing from the link alone.

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
   - **Addressed signaling**: `Offer`/`Answer`/`IceBatch` carry the recipient session pubkey in `to` (inside the encrypted payload); `NostrRelayPool` drops signals addressed to someone else. Through the relays they travel as `RelaySignal::Sealed`: sealed with ECDH to the recipient's session key inside the room-key encryption, so the room key alone opens only `Presence` and `PeerLeft` (SDP and ICE carry IP addresses). The lower pubkey of each pair dials; perfect negotiation (polite = higher pubkey) handles any later glare.
   - **Signed room messages**: every data-channel message is a `RoomEnvelope` signed by its author's session key (`sign_message`, domain-separated from Nostr event signatures) and encrypted with the room key. Receivers verify the signature **before** recording the message id for dedup, then relay it to neighbors without a direct link to the author (`Roster::forward_targets`). Attribution always comes from the verified `author`, never from a self-declared name.
   - **Protocol version**: members link only with members on the same `PROTOCOL_VERSION` (`protocol/src/version.rs`), so nothing exchanged needs to stay compatible across versions. Every relay signal is `VersionedSignal { v, payload }` inside the room-key encryption; `decode_signal` reads `v` first and never parses another version's payload. The pool reports other versions instead of linking: the older side shows a reload banner (`#update-banner`), the newer side a one-time toast per member. Links form only through that check, so data-channel messages carry no version.
   - **When to bump `PROTOCOL_VERSION`**: any change to how signals or room messages are encoded (the `wire_format_matches_protocol_version` test fails until you bump it and record the new fingerprint), signed-bytes formats, the file chunk layout, or a rule every member must apply alike (caps, gossip, history, rekey). UI-only changes don't bump it, so deploying them never splits a live room. The shape of `v` itself must never change, and neither may a plain room's topic (`hash_room_topic`): versions only notice each other on a shared topic.
   - **Admin secret**: `admsk` exists only in the creator's admin link. The invite link, the QR code and **🔗 Copy Link** always use `invite_url()`, which strips it.
   - **Deterministic caps**: every member evaluates `Roster::evicted` / `latest_beyond_cap` on the same data; a member who finds itself evicted (room, voice seat or video slot) backs off on its own. Caps are enforced by honest clients, not cryptographically.
   - **Rekey**: only admin-signed `AdminRekey` envelopes are honored; grants are ECDH-sealed per recipient session key. Members offline during a rekey are stranded and need a fresh invite.
   - **History**: off unless `&hist=1`; only messages whose author's link had `hist=1` (`shareable`) are ever served, as signed originals verified by the receiver. RAM only; enforced by honest clients.
   - **Lounge media**: media flows only between members who both hold a voice seat over an open direct link (`sync_media_for`). Each link has at most one audio and one video `RtcRtpSender`; toggles use `replaceTrack`, never add/remove, so SDP does not grow.

6. **Hosting Anywhere (static, any path)**:
   - The production build must work at a domain root, under a path (`https://<user>.github.io/<repo>/`) and on IPFS: asset URLs are relative (`crates/client/Trunk.toml`), and code never navigates to `/`; use `state::page_base_url()` for "home".
   - The Service Worker only precaches files with fixed names (`./`, `./index.html`); hashed assets are cached on first fetch. Precaching a missing file makes its install fail on real hosts (the local dev server masks this by answering unknown paths with `index.html`).
   - `Cargo.lock` is committed so CI deploys the dependency versions that were tested (`--locked`).
   - **Host TURN (Cloudflare)**: the client fetches `./ice-servers` once when entering a room and adds any servers returned to `RtcConfiguration`; 404, HTML or a timeout falls back silently (STUN plus `&turn=`). The Worker keeps `TURN_KEY_API_TOKEN` secret, returns only short-lived credentials (`Cache-Control: no-store`), filters port-53 URLs, rejects cross-site requests and rate-limits per IP. The request never includes room data, and the Service Worker never caches it.
   - The Worker entry module (`worker/index.js`) may only export its default handler: the Workers runtime rejects other named exports (put helpers in `ice.js`).

7. **Relays (signaling only)**:
   - The room's relays are part of the room and travel in the link: `&relays=` lists them exactly (`nostr` = the public relays, `protocol::PUBLIC_RELAYS`); absent means public (or the dev server's own relay on localhost/LAN). The creator chooses at creation; joiners never change it, so every member can find the others.
   - Relays carry only discovery (`Presence`, `PeerLeft`) and each link's first handshake. Once a link's chat channel is open, its renegotiation (offer, answer, ICE, e.g. the first voice or camera track) goes over the link itself as a direct-only `LinkSignal` (`send_link_signal`). Members already linked keep chatting, calling and starting video through a relay outage; only newcomers and reconnecting links wait.
   - `NostrRelayPool` reconnects each relay with backoff (1 s doubling to 30 s, jitter, reset after a stable minute), resubscribes and re-announces presence; it is closed when the session leaves (including rekey migration).
   - `dchat-relay` must stay RAM-only and dchat-only: ephemeral kinds only, signature and freshness checks, per-connection rate limits, per-IP connection caps, optional origin allow-list, no IPs/topics/content in logs. The dev server mounts the same code at `/nostr`, so E2E runs exercise it.

8. **NAT Traversal (known limitation, must stay documented)**:
   - **STUN choice** (`protocol::plan_ice`): the room's `&stun=`, otherwise any STUN the host offers (`./ice-servers`), otherwise `FALLBACK_STUN_URL` (Google, which then sees members' IPs). Never add Google, or any other third party, when something else already does the job.
   - **Hide IP addresses** (`&hideip=1`, a create-form checkbox): `iceTransportPolicy: "relay"`, no STUN at all, so members see only the TURN server's address. It needs TURN (host or `&turn=`); without one the room shows `#no-turn-banner`. It hides members from each other, not from relays or the TURN server.
   - Without TURN, ICE has STUN only. Pairs behind carrier-grade NAT (mobile data) or symmetric NAT often cannot link directly: roughly 10–20% of pairs, and more pairs fail as a room grows.
   - Such pairs show `via <name>` in the member list: **text** still flows, gossip-relayed through a mutual member (signed, room-key encrypted). **Audio, video and files** need a direct link and are unavailable for that pair. With no mutual member the person stays `connecting…`.
   - The fix is an optional TURN server supplied in the URL fragment (`&turn=…&turnuser=…&turnpass=…`, see `build_rtc_config` in `session.rs`). TURN relays encrypted packets only; it sees IPs and timing, never content.
   - The README section "When Members Can't Connect Directly (NAT)" is the user-facing version; keep both in sync. `docs/PRIVACY.md` lists who sees what: update it whenever data flows change.

9. **Remote Control (dchat-host)**:
   - **Only by the sharer's click**: `ControlRequest` waits in `ControlState` until the sharer allows it; nothing is ever granted automatically. One member holds mouse and keyboard at a time (granting moves it, `TakenOver`); controllers take slots P1–P4. Grants end with the share (or a share that isn't a whole monitor), the app pairing, the viewer's voice seat, the link, and the session (`set_available(false)`, `on_control_peer_lost`, `on_control_voice_state`).
   - **Input path**: viewer → sharer only over their direct link's `input-events` (reliable) and `input-state` (unordered, no retransmits) channels, sealed with the room key and an AAD binding lane, seq, sender and recipient (`seal_input`). Never relayed. The sharer's tab opens, budgets (`InputBudget`) and filters (`InputGate`: only what that member holds, pads mapped to their slot) before forwarding to the app, which checks roles again.
   - **dchat-host**: listens on 127.0.0.1 only; Host header must be its own loopback port (DNS rebinding); Origin must be in a never-empty allow-list; pairing needs the one-time code printed in its terminal, proven by HMAC both ways (the tab sends nothing to an app that can't prove it), with lockout after 5 wrong codes; one connected session at a time (a disconnected one is replaced by a new pairing). It releases everything held on every exit path (revoke, `ReleaseAll`, socket loss, `Bye`, stop, watchdog after 1.5 s without input, Drop), never logs input, and writes no files.
   - **Tab side**: the app link opens only when the user clicks Connect; the code and session token live in RAM. Mouse and keyboard can do anything the sharer can (including clicking Allow for others): the prompt says so.
   - **Versions**: the tab ↔ app messages have their own `AGENT_PROTOCOL_VERSION` and fingerprint test (`protocol/src/agent.rs`); room messages and input packets are in `PROTOCOL_VERSION`.

---

## 2. Repository Layout

```
dchat/
├── Cargo.toml                  # Workspace root (protocol, relay, host-agent, server, client)
├── crates/
│   ├── protocol/               # Shared types, messages, ChaCha20-Poly1305 crypto
│   │   └── src/
│   │       ├── agent.rs        # Tab ↔ dchat-host contract: JSON messages, mutual pairing proofs, own version + fingerprint
│   │       ├── control.rs      # Remote-control permissions: ControlState (one mouse/keyboard holder, pads P1–P4), InputGate (unit-tested)
│   │       ├── crypto.rs       # 256-bit keygen, encrypt/decrypt, base64 helpers
│   │       ├── fragment.rs     # Order-preserving URL fragment parser (keeps unknown params)
│   │       ├── input.rs        # Remote-control input events, binary codec, sealed packets (room key + AAD), capture helpers
│   │       ├── keycodes.rs     # DomCode: KeyboardEvent.code ↔ USB HID usage (126 keys)
│   │       ├── messages.rs     # Addressed SignalPayload, signed RoomEnvelope/RoomBody, ICE types
│   │       ├── nostr.rs        # NIP-01/16 types, BIP-340 Schnorr keys (k256), message signing, topic hashing
│   │       ├── relays.rs       # Room relay list: &relays= parsing (exact, `nostr` = public), validation
│   │       ├── password.rs     # Room passwords: Argon2id stretch, password room key and topic (unit-tested)
│   │       ├── room.rs         # Roster, link graph, gossip routing, member/voice/video cap eviction, RoomParams (pure, unit-tested)
│   │       ├── version.rs      # PROTOCOL_VERSION, versioned relay signals, wire-format fingerprint test
│   │       └── lib.rs
│   ├── relay/                  # dchat-relay: RAM-only Nostr relay for dchat signaling (lib + binary)
│   │   ├── Dockerfile          # Container image (build from the repo root)
│   │   ├── src/
│   │   │   ├── limits.rs       # Policy: kind 20001 only, d tag, freshness, signature, token bucket (unit-tested)
│   │   │   ├── hub.rs          # Subscriptions and fan-out, per-IP connection counts (RAM only)
│   │   │   ├── connection.rs   # NIP-01 REQ/EVENT/CLOSE per WebSocket, pings, limits
│   │   │   ├── lib.rs          # axum router: WebSocket at /, NIP-11 information document
│   │   │   └── main.rs         # CLI / env config, graceful shutdown, IP-free logs
│   │   └── tests/relay.rs      # Integration tests over real WebSockets
│   ├── host-agent/             # dchat-host: remote-control companion app on the shared computer (lib + binary)
│   │   ├── dist/               # 60-dchat-host.rules (udev: /dev/uinput for the seat user), uinput.conf
│   │   ├── src/
│   │   │   ├── server.rs       # ws://127.0.0.1 only: Host + Origin checks, mutual pairing, one session, resume, limits
│   │   │   ├── pairing.rs      # One-time codes, lockout, session token (pure, unit-tested)
│   │   │   ├── engine.rs       # One thread owns the injector and everything held; releases on every exit path; watchdog
│   │   │   ├── inject/         # Injector trait; linux.rs (uinput keyboard, absolute pointer, relative mouse); windows.rs (SendInput); win_input.rs (SendInput records, unit-tested everywhere); mock.rs (recording)
│   │   │   ├── geometry.rs     # Shared-monitor choice, desktop-wide absolute coordinates (unit-tested)
│   │   │   ├── keymap.rs       # DomCode → evdev and Windows scan codes (exhaustive, unit-tested)
│   │   │   ├── monitors/       # Monitor layout: X11 RandR (also through XWayland), Windows EnumDisplayMonitors (physical pixels)
│   │   │   └── config.rs, status.rs, lib.rs, main.rs
│   │   └── tests/              # agent.rs (WebSockets + recording injector), uinput.rs (real devices where /dev/uinput is writable)
│   ├── server/                 # Axum dev server: static files + dev TLS + dchat-relay at /nostr
│   │   └── src/
│   │       ├── main.rs         # CLI args, LAN IP detection, QR code banner, /nostr (crates/relay) & /ws routing
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
│           ├── agent.rs        # Link to dchat-host: connects on click only, mutual pairing, resume; code and token in RAM
│           ├── i18n.rs         # Strongly typed i18n, browser detection, RTL handling
│           ├── ice.rs          # Optional host TURN: GET ./ice-servers (3 s timeout, silent fallback)
│           ├── layout.rs       # Video grid fit: largest 16:9 tiles without scrolling (unit-tested)
│           ├── media.rs        # Capture (mic/camera/screen), per-member <audio>, video attach, speaking meter
│           ├── mesh.rs         # PeerLink: one RTCPeerConnection per member (chat + file-transfer channels), perfect negotiation, batched ICE, tracks
│           ├── names.rs        # Random session names, name sanitizing, pubkey tags
│           ├── nostr_pool.rs   # Multi-relay pool, fan-out broadcast, deduplication, recipient filtering
│           ├── qr.rs           # On-the-fly SVG QR code generation
│           ├── remote_input.rs # Viewer capture over a shared screen: pointer (letterbox-aware), keys, wheel, heartbeat
│           ├── session/
│           │   ├── mod.rs      # RoomSession: mesh orchestration, signed gossip, roster, caps, e2e hooks
│           │   ├── admin.rs    # Kick / rotate link: ECDH-sealed AdminRekey, migration to the new room
│           │   ├── control.rs  # Remote control: requests and grants, input gate to dchat-host, sealed input as a viewer
│           │   ├── extras.rs   # Typing, reactions, edit/delete, ECDH-sealed DMs, @mention detection
│           │   ├── files.rs    # Room-wide file cards, per-requester pulls, upload queue, chunk I/O
│           │   ├── history.rs  # Opt-in history (&hist=1): signed shareable messages for late joiners
│           │   └── lounge.rs   # Voice lounge: seats, per-link senders, voice/video caps, speaking, controls
│           └── state.rs        # UI types, URL fragment helpers (create room, invite/admin links), relays
├── e2e/                        # Playwright automated multi-peer end-to-end tests
│   ├── playwright.config.js    # Starts the dev server (serves crates/client/dist-e2e) and a recording dchat-host on port 7499
│   └── tests/
│       ├── helpers.js          # createRoom / joinRoom / memberRow helpers shared by specs
│       ├── group_chat.spec.js  # 3-member mesh, fan-out, caps + admin seat, relayed text without a direct link
│       ├── group_voice.spec.js # 3-member lounge: join prompt, mesh audio, mute state, speaking, voice/video caps
│       ├── group_admin.spec.js # Kick (removed screen, others migrate with chat), rotate link (voice rejoin, old invite stranded)
│       ├── group_history.spec.js # hist=1 backlog incl. departed authors' names, default off, per-author shareable flag
│       ├── group_extras.spec.js  # Typing, reactions, edit/delete, direct + relayed DMs (relay can't read), @mention highlight
│       ├── p2p_chat.spec.js    # 2-member room, E2EE message exchange, reload wipe, fragment params
│       ├── host_turn.spec.js   # Host-offered TURN (./ice-servers) used; 404 falls back to STUN; no room data sent
│       ├── relay_choice.spec.js# Relay choice at room creation: public default, my relay only, my relay + public
│       ├── relay_reconnect.spec.js # Dropped relay connection reconnects; newcomers still reach the member
│       ├── link_renegotiation.spec.js # With every relay cut after linking, voice and video still negotiate over the link
│       ├── protocol_version.spec.js # Different protocol versions never link; the older member gets a reload banner
│       ├── room_password.spec.js # Password rooms: link + password, wrong password finds nobody, rekey keeps the password
│       ├── remote_control.spec.js # dchat-host pairing, request/allow, clicks through letterboxing, one holder, others' raw input dropped, release on shortcut/revoke/link loss, prompts in fullscreen, window shares not offered, game mode (pointer lock, relative moves, motion hint)
│       ├── privacy.spec.js     # Sealed handshakes vs the room key, STUN fallback, hideip through a real TURN (node-turn)
│       ├── audio_call.spec.js  # 2-member lounge audio, mic/speaker mute, leave (replaceTrack null) and rejoin
│       ├── video_call.spec.js  # Camera tiles, camera flip keeps the mic, camera off, grid teardown
│       ├── screen_share.spec.js# Screen share, switch to camera on the same sender, stop
│       ├── file_sharing.spec.js# Room-wide file cards: parallel pulls, decline/withdraw, upload queue, unreachable sender, sender leaving
│       ├── audio_settings.spec.js # Mic processing checkboxes, live track swap in voice, carry-over, reload reset
│       └── i18n.spec.js        # UI localization, dynamic switching, Arabic RTL, zero persistence
├── .github/workflows/
│   ├── build.yml               # Reusable: Rust + Worker tests, release build, no-hooks check, "site" artifact
│   ├── pages.yml               # Deploy the site to GitHub Pages on push to main
│   ├── cloudflare.yml          # Deploy to Cloudflare Workers (skips until CLOUDFLARE_* secrets exist)
│   ├── relay-image.yml         # Publish ghcr.io/<owner>/dchat-relay when the relay changes
│   └── host-agent.yml          # dchat-host tests and release build on Linux and Windows
├── deploy/relay/               # docker-compose.yml + Caddyfile (automatic wss://), dchat-relay.service (systemd)
├── wrangler.jsonc              # Cloudflare Worker: static assets + worker/, ICE_LIMITER rate limit
├── worker/
│   ├── index.js                # Entry: GET */ice-servers → ice.js, everything else → static assets
│   ├── ice.js                  # Fresh Cloudflare TURN credentials (token stays server-side), port-53 filter
│   └── index.test.mjs          # node --test worker/ (stubbed TURN API)
├── docs/DEPLOYMENT.md          # Step-by-step hosting: GitHub Pages, Cloudflare Workers + TURN, troubleshooting
├── docs/PRIVACY.md             # Who sees what (members, relays, STUN/TURN, host), protections, possible improvements
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
  - **8e Chat extras (Completed)**: `Typing` (throttled 3 s, expires after 4.5 s); `Reaction{target, emoji, on}` limited to `REACTIONS`, latest toggle per member wins (`Reactions`, unit-tested); `Edit`/`Delete` honored only from the original author (`message_authors`), and they drop the message from history; `Dm{sealed}` sealed with `seal_json` (ECDH session keys), sent only over the direct link when there is one, otherwise gossiped; it names no recipient (protocol v3): every member tries to open it, only the recipient can, and the recipient never relays it; `mentions()` with word boundaries on both sides; chime via Web Audio; `(n)` title badge while hidden; DM threads end when the peer leaves.

- **Phase 9: Remote Control (in progress)**
  - **9a Desktop control on Linux (Completed)**: protocol v4 (`ControlStatus`/`ControlRequest`/`ControlGrant`/`ControlRelease`, sealed input packets, `DomCode`); `dchat-host` (loopback WebSocket, mutual pairing, engine with release-on-exit and watchdog, uinput backend, X11/XWayland monitor layout); tab ↔ app link; permission prompts that follow fullscreen; desktop-mode mouse and keyboard with letterbox-aware positions; E2E against a recording dchat-host.
  - **9b Windows and game mode (Completed)**: `SendInput` backend (scan codes, `VIRTUALDESK` absolute positions, per-monitor DPI awareness, administrator note), Windows monitor layout, `win_input` records unit-tested on every platform, CI on Windows; viewer game mode (pointer lock with `unadjustedMovement`, relative moves, keyboard lock in fullscreen, losing the lock ends control), `Mode` switches the sharer's stream to `contentHint: motion` at 60 fps.
  - **9c controllers**, **9d releases and polish**: planned.

---

## 4. Verification Loop for AI Agents

Whenever making changes, an agent must execute the following test matrix before marking work as complete:

### Step 1: Protocol & Server Unit Tests
Verify cryptographic primitives, serialization, and room lifecycle in memory:
```bash
cargo test --workspace
```
*Expected: 100% pass, 0 failures, 0 ignored.* This includes `host-agent`; its `tests/uinput.rs` creates real input devices only where `/dev/uinput` is writable and passes (saying so) elsewhere.

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

And that dchat-host still compiles for Windows (`rustup target add x86_64-pc-windows-gnu` once; CI runs its tests on Windows):
```bash
cargo check -p host-agent --target x86_64-pc-windows-gnu --all-targets
```

### Step 3: Trunk Frontend Build
Rebuild the production bundle and the E2E bundle (with `e2e-hooks`, never deployed):
```bash
make build-client build-client-e2e
```
*Expected: `crates/client/dist/` and `crates/client/dist-e2e/` populated with `client-..._bg.wasm`, `client-...js`, and assets. `grep -c __dchat crates/client/dist/*.wasm` must print `0`.*

Worker unit tests (Cloudflare TURN endpoint, stubbed API):
```bash
node --test worker/
```
*Expected: all pass.* For Worker changes, also validate in the real runtime: `npx wrangler@4 deploy --dry-run` and `npx wrangler@4 dev` (Node.js 22+).

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
- [ ] `dchat-relay` stores nothing and logs no IP addresses, room topics or content; it only accepts signed, fresh, ephemeral dchat events.
- [ ] Relay URLs from links or the create form pass `is_relay_url` (no `&`, `#`, `,` or whitespace), and HTTPS pages only use `wss://`.
- [ ] The Cloudflare TURN API token exists only as a Worker secret; `/ice-servers` responses are short-lived, `no-store` and never cached by the Service Worker; the client request carries no room data.
- [ ] `e2e-hooks` code (`window.__dchat.selfPubkey/blockPeer/throttleUploads`, and reading `window.__dchatProtocolVersion`) stays behind `#[cfg(feature = "e2e-hooks")]` and out of `make build-client` output.
- [ ] Offers, answers and ICE reach the relays only as `RelaySignal::Sealed` (never readable with the room key alone).
- [ ] No third-party server is contacted by default when the room or host provides one (Google STUN only via `plan_ice`'s fallback); `hideip` rooms gather relay candidates only.
- [ ] Wire or shared-rule changes bump `PROTOCOL_VERSION` and record the new fingerprint; signals from other versions are reported, never parsed or linked.
- [ ] `LinkSignal` is applied only from the link's own remote (direct-only), and only for offers, answers and ICE addressed to us.
- [ ] Direct-only room messages (`RoomBody::recipient()` is `Some`) are applied only when `to` is us and the envelope came straight from its author; they are never relayed.
- [ ] File chunks are accepted only from the offer's author over that author's own link, strictly in order; anything else aborts or is dropped.
- [ ] `AdminRekey` is honored only from a member whose Hello carried a valid admin proof; grants are sealed per recipient (never the room key in clear).
- [ ] Remote control: nothing is granted without the sharer's click; input is accepted only from current holders, over their own direct link, sealed with the input AAD; dchat-host stays loopback-only with Host/Origin checks and mutual pairing, releases held input on every exit path, and logs no input.
- [ ] DM plaintext is only ever sealed with `seal_json` to the recipient's session key; DMs carry no recipient field; the recipient never relays a DM; no DM text in logs.
- [ ] A room password never appears in the link, logs, messages or storage: only its salt (`pw`) is in the link, the input is cleared after stretching, and only the stretched value stays in RAM.
- [ ] Text inputs and lobby forms keep `autocomplete="off"`; message boxes follow the spell-check setting.
- [ ] Edits/deletes are applied only when the envelope author equals the original message author.
- [ ] History serves only `shareable` chat envelopes (author's choice) and the receiver verifies every signature; Hellos from history only label names, never join the roster.
