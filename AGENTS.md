# AGENTS.md — AI Development & Verification Guide for dchat

`dchat` is an ephemeral, zero-knowledge, peer-to-peer WebRTC chat application written in pure Rust (compiled to `wasm32-unknown-unknown` without Emscripten). The application stores no messages on disk or persistent browser storage, relying solely on WebAssembly linear memory that vanishes on tab reload.

All automated and AI-driven development in this repository must adhere to the invariants, architecture rules, and testing requirements documented here.

---

## 1. Core Architectural Invariants

Every agent modifying this codebase must enforce these non-negotiable security and privacy invariants:

1. **Zero Persistence Invariant**:
   - **Forbidden APIs**: `localStorage`, `sessionStorage`, `indexedDB`, document `cookie`, or server-side disk databases.
   - **Rule**: All session state, encryption keys, and chat messages must reside strictly in Rust struct fields and reactive signals within WebAssembly linear memory.
   - **Verification**: Reloading the page or closing the tab must completely wipe that tab's copy of the conversation (members still in the room keep theirs in RAM and sync it back on re-entry; nothing remains once the last member leaves). Automated E2E tests check that `localStorage.length === 0` and `sessionStorage.length === 0`.

2. **Zero-Knowledge Key Distribution**:
   - **URL Hash Confidentiality**: The room ID and 256-bit symmetric encryption key are stored in the URL fragment (`#room=<id>&key=<base64_secret>`).
   - **Rule**: URL hash fragments are never sent to the server over HTTP requests or WebSocket handshakes. The server is completely blind to the decryption key.
   - **Links from outside the page** (the lobby's **Join with a link**, and `launchQueue` launches in the installed app) go through `FragmentParams::from_link` (only the part after `#`, any site's link) and `state::join_link` (rewrite the fragment in place, then reload). The manifest has no `protocol_handlers`: their `%s` URL template would carry the key in a request.
   - **Room passwords** (`&pw=<salt>`, `protocol::password`): room key = SHA-256(link key, Argon2id(password, salt)), relay topic = `password_room_topic` (so the link alone doesn't find the room). The stretched value stays in RAM and re-derives the key after a rekey (grants carry link keys, never derived keys). Never put a password verifier in the link: it would allow offline guessing from the link alone.

3. **Dual-Layer End-to-End Encryption & Decentralized Signaling**:
   - **WebRTC Transport**: DTLS encryption for `RTCDataChannel` (text) and DTLS-SRTP for audio/video media streams.
   - **Application-Layer**: All signaling payloads (SDP offers/answers, ICE candidates) and `RTCDataChannel` messages are encrypted using **ChaCha20-Poly1305** with unique 96-bit nonces.
   - **Decentralized Nostr Relay Fabric**: Signaling is relayed over decentralized Nostr relays using **NIP-16 Ephemeral Events (Kind 20001)** signed in RAM by disposable BIP-340 Schnorr burner keys (`k256`): a relay key fresh every session, certified inside each encrypted frame by the member's identity (`RelayFrame { from, cert }`, `relay_key_message`), so relays can't link a room to the one it was rotated from. Room topics are hashed via SHA-256 (`["d", sha256(room_id)]`), ensuring relays and eavesdroppers remain 100% blind to room IDs, encryption keys, and message contents with zero disk persistence.

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
   - **File links (Phase 12)**: an uploader may add up to `MAX_FILE_CONNECTIONS` − 1 (7) extra `RTCPeerConnection`s to the downloader (`mesh::FileLink`): one negotiated `file-transfer` channel, no media, negotiated only over the open main link with the direct-only `FileLinkSignal`, the room's `RtcConfiguration` (so `hideip` stays relay-only), closed with the main link or 15 s after the last upload to that member. Only the dialer sends chunks on a file link; a member refuses more than 7 from one dialer.
   - **Parallel transfer rules** (`protocol::transfer`, part of the wire fingerprint): the downloader puts chunks back in order (`Reorder`), keeps at most `SEND_WINDOW_CHUNKS` (512) ahead of the first missing one and drops the rest; it confirms progress with an unsigned acknowledgement on the main link's file channel, in the chunk layout (a chunk header about the uploader's own file, `chunk_index` = next, no data), at most every `ACK_EVERY_CHUNKS` (128) or `ACK_EVERY_MS`. Keep acknowledgements rare: packets flowing back on the connection slowed Chrome's SCTP by a third when sent every 16 chunks. The uploader never sends past the window, resends from the last confirmed chunk when a file link is lost, and counts an upload as delivered only once every chunk is confirmed. When the main link drops, the downloader keeps its download (sinks, `Reorder`, an open save stream) as `Paused` and, once a link to the author opens again, asks for the rest with `FileRequest { from_chunk: Reorder::next }`; the uploader starts that run there (`acked = from_chunk`), and a new request for a run still active replaces it. A paused download ends when its author leaves or stays away past the grace. How many links it opens is its own choice (`LinkGrowth`: settle, add one, keep it only if the rate rose by more than 10 %, with a second look), up to the local cap in ⚙️ Settings → Files.
   - **Addressed signaling**: `Offer`/`Answer`/`IceBatch` carry the recipient session pubkey in `to` (inside the encrypted payload); `NostrRelayPool` drops signals addressed to someone else, and any frame whose relay key isn't certified by the identity it claims (checked once per relay key). Through the relays they travel as `RelaySignal::Sealed`: sealed with ECDH to the recipient's session key inside the room-key encryption, so the room key alone opens only `Presence` and `PeerLeft` (SDP and ICE carry IP addresses). The lower pubkey of each pair dials; perfect negotiation (polite = higher pubkey) handles any later glare.
   - **Signed room messages**: every data-channel message is a `RoomEnvelope` signed by its author's session key (`sign_message`, domain-separated from Nostr event signatures) over `("dchat:envelope:v2", adm, id, author, ts, body)`, and encrypted with the room key. Binding the room's admin pubkey (`adm`, unchanged by rekeys; `""` without one) stops a member of two rooms from replaying one room's messages or names into the other; receivers verify with their own `adm`. Receivers verify the signature **before** recording the message id for dedup, then relay it to neighbors without a direct link to the author (`Roster::forward_targets`). Attribution always comes from the verified `author`, never from a self-declared name.
   - **Protocol version**: members link only with members on the same `PROTOCOL_VERSION` (`protocol/src/version.rs`), so nothing exchanged needs to stay compatible across versions. Every relay signal is `VersionedSignal { v, payload }` inside the room-key encryption; `decode_signal` reads `v` first and never parses another version's payload. The pool reports other versions instead of linking: the older side shows a reload banner (`#update-banner`), the newer side a one-time toast per member. Links form only through that check, so data-channel messages carry no version.
   - **When to bump `PROTOCOL_VERSION`**: any change to how signals or room messages are encoded (the `wire_format_matches_protocol_version` test fails until you bump it and record the new fingerprint), signed-bytes formats, the file chunk layout, or a rule every member must apply alike (caps, gossip, the chat log's rules and sync, rekey). UI-only changes don't bump it, so deploying them never splits a live room. The shape of `v` itself must never change, and neither may a plain room's topic (`hash_room_topic`): versions only notice each other on a shared topic.
   - **Admin secret**: `admsk` lives only in admin links (the creator's, and the link of a member who took over or was made admin) and, sealed with ECDH to one member's session key, inside `AdminHandover`. A dormant heir holds it in RAM only, never in its link. The invite link, the QR code and **🔗 Copy Link** always use `invite_url()`, which strips it.
   - **Admin succession (Phase 14)**: `protocol::succession`, part of the wire fingerprint. Seniority is each tab's own first sighting of a member (`FirstSeen`), behind a previous admin's ranking it inherited with the secret; never the self-declared `join_ts`. The maintainer (lowest-pubkey reachable admin) hands the secret to the heir (most senior non-admin, present or away; never handed to an away one, which gets it once back) as a gossiped `AdminHandover { promote: false }` with no recipient field (every member tries to open it; `open_handover` keeps it only if it is `adm`'s secret), again whenever the heir changes, and never during the 5 s hold after a migration. It is checked once the current message is handled (`schedule_heir_check`), so a handover never overtakes the Hello that made us an admin. A grant is honored only from a roster admin and only by non-admins; the newest `(ts, id)` wins, so an older heir drops its copy and a stale grant never brings it back. The heir takes over once no admin has been present or away for `ADMIN_ABSENT_MS` (15 s) while another member is reachable (alone, it may be its own network that dropped; away members are no company): it writes `admsk` into its link, re-publishes its Hello with an admin proof (receivers accept the newer Hello and show `Notice::NowAdmin`), then maintains the next heir. **Make admin** sends `AdminHandover { promote: true }`: the recipient becomes an admin at once; it can't be undone (admins can't kick admins). Enforced by honest clients: a holder of the secret could use it any time.
   - **Deterministic caps**: every member evaluates `Roster::evicted` / `latest_beyond_cap` on the same data; a member who finds itself evicted (room, voice seat or video slot) backs off on its own. Caps are enforced by honest clients, not cryptographically.
   - **Rekey**: only admin-signed `AdminRekey` envelopes are honored; grants are ECDH-sealed per recipient session key, for present and away members alike. Every member who follows a rekey while someone was away keeps its old relay pool (not the session) open until the last of those absences would expire (`Forwarding`), and answers an away member's `Presence` on the old topic with `SignalPayload::RekeyForward { to, envelope }`, sealed to them like any addressed relay signal (jittered, once per 10 s per member); the returner verifies the envelope with its own `adm`, requires a roster admin author, and follows it through the usual `on_admin_rekey` (or shows the removed screen if kicked). A closed session's pool never touches the UI. Members offline longer than the grace during a rekey are stranded and need a fresh invite. The next session gets `SessionCarry` (RAM only): the same identity (so authors keep editing and deleting their earlier messages), the files we offered (still downloadable), the seniority order (`FirstSeen`, so the same heir is picked again), and the app-level chat log; relay events get a fresh key. A dormant admin secret is never carried: the maintainer hands it again after the hold.
   - **History (always on, Phase 13)**: every member keeps the room's chat log (`protocol::chat_log::ChatLog`, app level in `main.rs`, so a rekey carries it; a fresh entry or the removed screen empties it). It holds signed originals only: chat messages and file cards (thread roots, written once), the root author's latest edit or withdrawal, each member's latest reaction toggle, deletions as tombstones (the original dropped for good; one by anyone but the author is ignored), and each author's latest Hello (names only, never the roster). Never DMs, notices or direct-only bodies. Every rule gives the same log whatever order envelopes arrive in. Deterministic budget: `CHAT_LOG_BUDGET` (32 MiB) of serialized envelopes, oldest threads by `(ts, id)` dropped first, nothing below the horizon taken back. Envelopes dated more than `MAX_FUTURE_SKEW_MS` (5 min) ahead are shown live but not logged (yet). Sync (`session/sync.rs`): on every new link each side pulls from the other, one peer at a time (`SyncQueue`): day-window summary, asks down to hours and minutes where digests (count + XOR of `entry_fingerprint`) differ, wants of ≤ `SYNC_WANT_MAX` ids, batches sent only while the chat channel has room (`PeerLink::wait_for_chat_room`). `Sync*` bodies are direct-only; batches are accepted only from the active pull peer, only for ids asked for (Hellos come along), and every envelope is verified with our own `adm` before it is merged. Constants and rules are in the wire fingerprint. Enforced by honest clients: anyone who gets in with the link (and password) can read what the room still holds.
   - **Away and return (Phase 16)**: `protocol::presence`, part of the wire fingerprint. A member who drops out of reach (`Roster::reachable_from`) without `Leave`/`PeerLeft` is away for `AWAY_GRACE_MS` (5 min), counted on each tab's own clock (`Absences`), then gone (the "left" line, cards `SenderLeft`, paused downloads end, waiting DMs `NotDelivered`, DM thread ended); back within the grace there is no notice at all, back after it is a `Joined`. Caps still count reachable members only (latest-loses: a returner keeps its seat). Away members are listed (`LinkUi::Away`), can be kicked, never made admin, count for succession (see above) and get rekey grants. A tab finds out it was paused when its ticker misses `FROZEN_GAP_MS` (8 s), checked on every tick and on `visibilitychange`, and on `online`: it calls `NostrRelayPool::wake` (re-sends its subscription, replaces connections silent for 3.5 s, skips reconnect backoff), clears `retry_after`, beacons fast again, sends a direct-only `LinkCheck` over each link it holds and drops the ones that don't answer within `LINK_CHECK_MS`, and re-captures a mic the browser ended. An offer or ICE through the relays from a member whose link to us has been open over `STALE_LINK_MS` means they lost it: ours is dropped and replaced. `retry_after` only delays dialing a member still reachable through others (never one who is away, gone or just came back). DMs to an away member are sealed and signed at once and wait in RAM (`dm_outbox`), flushed over the link (or gossip) when they are back. A reload is still a new identity: nothing about a member leaves RAM.
   - **Media previews & voice messages (Phase 15)**: `FileOffer.media` (`protocol::media::MediaInfo`: kind, pixel size, length, a ≤ `THUMB_MAX_BYTES` WebP/JPEG thumbnail drawn from the picture, a `WAVEFORM_BARS` waveform for voice) is checked by `RoomBody::is_well_formed` before anything shows it, and the chat log refuses a malformed one (`Ignored::Malformed`), so every member keeps the same cards. Previews only for `preview_type`'s allow-list (never SVG: a `blob:` URL has dchat's origin), built into a Blob of the allow-listed type, never the declared one; what loaded but fails to open is retried under fresh `blob:` addresses (`HeldMedia::book_refresh`, stale elements ignored), then keeps its card with the browser's reason (`MediaError`) and Download from the copy in RAM, or releases a copy that can no longer be read (`give_up`, `readable`). Files stay author-served (no mirrors): a viewer pulls with `load_media` (no save picker, so cards load media ≤ `AUTO_LOAD_BYTES` by themselves once on screen, IntersectionObserver), up to `INLINE_MAX_BYTES`; the bytes go to the app-level `HeldMedia` (RAM, carried across rekeys, emptied on the removed screen, least recently viewed released past `MEDIA_BUDGET_BYTES`, never what is playing or in the viewer). Download saves the held copy (`save_blob`): nothing is pulled again. Voice messages (`recorder.rs`, app level) are MP3 Blobs in RAM (an AudioWorklet loaded from a Blob taps the mic; `voice_mp3` resamples to 24 kHz and encodes 48 kbps with `rusty_mp3`, pure Rust), offered with `share_file` like any file, always through a review (never sent unasked); recording mutes the lounge mic and restores it. Reactions update a row in place (no `rev` bump), so a playing video or voice message is never rebuilt. The 16/200/512 MB limits, the 15-minute cap and the recording format are local: not in the wire fingerprint.
   - **Lounge media**: media flows only between members who both hold a voice seat over an open direct link (`sync_media_for`). Each link has at most one audio and one video `RtcRtpSender`; toggles use `replaceTrack`, never add/remove, so SDP does not grow.
   - **Video quality presets (Phase 10)**: the sharer picks one camera preset and one screen preset (`protocol::video`) for everyone; they are local settings, never on the wire, so they need no `PROTOCOL_VERSION` bump. `session/quality.rs` is the only place they are applied:
     - every `setParameters` writes the complete state (`maxBitrate`, `maxFramerate`, `scaleResolutionDownBy`, top-level `degradationPreference`, `encodings[0].codec`), deleting a key the preset leaves to the browser, with no `await` between `getParameters` and `setParameters`, one call per sender at a time;
     - `applyConstraints` always gets the full constraint set (it replaces the previous one) and never `frameRate.min` (Chrome's zero-hertz mode delays frames);
     - `contentHint` is set before the track is attached (Safari reads it only at attach); a live change re-attaches with `replaceTrack(sameTrack)`;
     - the codec comes from the preset's ladder (`codec_ladder`, `MediaCapabilities.encodingInfo`) and is picked among the link's negotiated codecs (`pick_negotiated`); a link that lacks it keeps its default;
     - the screen's `ScreenInfo` (sent to dchat-host to find the monitor) is read before any size cap: always the source size.
   - **No lip sync for video**: the video sender is added under its own empty `MediaStream` (`video_msid`), so receivers never hold video back to match the voice buffer. Viewers re-sync camera tiles only, by setting that video receiver's `jitterBufferTarget` to the member's current audio buffer delay every 2 s; screen tiles stay `null`. The receiver always builds its own per-member `MediaStream`.

6. **Hosting Anywhere (static, any path)**:
   - The production build must work at a domain root, under a path (`https://<user>.github.io/<repo>/`) and on IPFS: asset URLs are relative (`crates/client/Trunk.toml`), and code never navigates to `/`; use `state::page_base_url()` for "home".
   - The Service Worker only precaches files with fixed names (`./`, `./index.html`, `./manifest.webmanifest`, `./icons/*`); hashed assets are cached on first fetch. Precaching a missing file makes its install fail on real hosts (the local dev server masks this by answering unknown paths with `index.html`). The manifest and icons are served cache-first: bump `CACHE_NAME` when they change.
   - **Installed app (Phase 11)**: `manifest.webmanifest` keeps `id`, `start_url` and `scope` at `./` (so every mirror is its own app), `display: standalone` (a reload button would wipe the room) and `launch_handler.client_mode: ["navigate-new", "focus-existing"]`: desktop opens each room link in a new window; a single-window app (Android) gets it through `launchQueue`, and the page asks before leaving a live room. The install offer and the Apple hint appear only on the create screen (no room in the address).
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
   - **Only by the sharer's click**: `ControlRequest` waits in `ControlState` until the sharer allows it; nothing is ever granted automatically. One member holds mouse and keyboard at a time (granting moves it, `TakenOver`); controllers take slots P1–P4, limited to what the app reports it can create, and a pad with no update for 0.5 s goes back to neutral. Grants end with the share (or a share that isn't a whole monitor), the app pairing, the viewer's voice seat, the link, and the session (`set_available(false)`, `on_control_peer_lost`, `on_control_voice_state`).
   - **Input path**: viewer → sharer only over their direct link's `input-events` (reliable) and `input-state` (unordered, no retransmits) channels, sealed with the room key and an AAD binding lane, seq, sender and recipient (`seal_input`). Never relayed. The sharer's tab opens, budgets (`InputBudget`) and filters (`InputGate`: only what that member holds, pads mapped to their slot) before forwarding to the app, which checks roles again.
   - **Input pacing** (`protocol::pacing`): viewers send each pointer move at once, at most every 4 ms (`MoveSpacer`, ≤ 250 Hz). The sharer's `AgentPacer` keeps the frames sent to dchat-host under its 500 messages/s limit: state events (pointer, pads) are coalesced per member within 400 frames/s; clicks, keys and other events never wait and carry that member's pending state ahead of them, so order is kept.
   - **dchat-host**: listens on 127.0.0.1 only; Host header must be its own loopback port (DNS rebinding); Origin must be in a never-empty allow-list; pairing needs the one-time code printed in its terminal, proven by HMAC both ways (the tab sends nothing to an app that can't prove it), with lockout after 5 wrong codes; one connected session at a time (a disconnected one is replaced by a new pairing). It releases everything held on every exit path (revoke, `ReleaseAll`, socket loss, `Bye`, stop shortcut, Enter, Ctrl+C, SIGTERM/SIGHUP or Windows console close/logoff/shutdown, watchdog after 1.5 s without input, panic hook, Drop), never logs input, and writes no files.
   - **Tab side**: the app link opens only when the user clicks Connect; the code and session token live in RAM. The dialog's download link (`host_download_url`: `DCHAT_HOST_DOWNLOAD_URL`, else the building repository's latest release) opens with `rel="noopener noreferrer"`.
   - **Allowed site**: when none is built in or given and a console is attached, dchat-host asks; `origin_from_input` keeps only `scheme://host[:port]` (a pasted room link's key is dropped and never printed). Without a console it refuses to start. Mouse and keyboard can do anything the sharer can (including clicking Allow for others): the prompt says so.
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
│   │       ├── chat_log.rs     # The room's chat log: order-independent merge rules, 32 MiB budget, window digests, pull round, sync queue (pure, unit-tested)
│   │       ├── control.rs      # Remote-control permissions: ControlState (one mouse/keyboard holder, pads P1–P4), InputGate (unit-tested)
│   │       ├── crypto.rs       # 256-bit keygen, encrypt/decrypt, base64 helpers
│   │       ├── fragment.rs     # Order-preserving URL fragment parser (keeps unknown params), room links from any site (`from_link`)
│   │       ├── input.rs        # Remote-control input events, binary codec, sealed packets (room key + AAD), capture helpers
│   │       ├── keycodes.rs     # DomCode: KeyboardEvent.code ↔ USB HID usage (126 keys)
│   │       ├── media.rs        # Media previews: MediaInfo (thumbnail, size, length, waveform) and its limits, preview allow-list (no SVG), waveform bars, JPEG location removal (unit-tested)
│   │       ├── messages.rs     # Addressed SignalPayload, signed RoomEnvelope/RoomBody, ICE types
│   │       ├── nostr.rs        # NIP-01/16 types, BIP-340 Schnorr keys (k256), message signing, topic hashing
│   │       ├── relays.rs       # Room relay list: &relays= parsing (exact, `nostr` = public), validation
│   │       ├── pacing.rs       # Remote-control input pacing: MoveSpacer (viewer, 4 ms), AgentPacer (sharer → dchat-host budget), unit-tested
│   │       ├── password.rs     # Room passwords: Argon2id stretch, password room key and topic (unit-tested)
│   │       ├── presence.rs     # Away and return: who dropped out of reach, the 5-minute grace, back/expired/returned transitions (pure, unit-tested)
│   │       ├── succession.rs   # Admin succession: seniority (FirstSeen), maintainer, heir, grants, the 15 s takeover (pure, unit-tested)
│   │       ├── room.rs         # Roster, link graph, gossip routing, member/voice/video cap eviction, RoomParams (pure, unit-tested)
│   │       ├── transfer.rs     # Parallel file transfer: reorder buffer, send window, acknowledgement pace, rate meter, LinkGrowth (pure, unit-tested)
│   │       ├── version.rs      # PROTOCOL_VERSION, versioned relay signals, wire-format fingerprint test
│   │       ├── video.rs        # Video quality presets (local only): capture and sender targets, bitrate/scale math, codec ladder, WebCodecs adapter (unit-tested)
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
│   │   ├── dist/               # install.sh (Linux setup, --uninstall), 60-dchat-host.rules (udev: /dev/uinput for the seat user), uinput.conf, README.txt: all shipped in releases
│   │   ├── src/
│   │   │   ├── server.rs       # ws://127.0.0.1 only: Host + Origin checks, mutual pairing, one session, resume, limits
│   │   │   ├── pairing.rs      # One-time codes, lockout, session token (pure, unit-tested)
│   │   │   ├── engine.rs       # One thread owns the injector and everything held; releases on every exit path; watchdog
│   │   │   ├── inject/         # Injector trait; linux.rs (uinput keyboard, absolute pointer, relative mouse); windows.rs (SendInput); win_input.rs (SendInput records, unit-tested everywhere); mock.rs (recording)
│   │   │   ├── geometry.rs     # Shared-monitor choice, desktop-wide absolute coordinates (unit-tested)
│   │   │   ├── keymap.rs       # DomCode → evdev and Windows scan codes (exhaustive, unit-tested)
│   │   │   ├── pad_map.rs      # Standard pad state → XInput report (ViGEm) and xpad evdev events (uinput), unit-tested
│   │   │   ├── monitors/       # Monitor layout: X11 RandR (also through XWayland), Windows EnumDisplayMonitors (physical pixels)
│   │   │   ├── stop.rs         # Global stop shortcut Ctrl+Alt+Shift+Q (Windows, X11; reported unavailable on Wayland)
│   │   │   └── config.rs, status.rs, lib.rs, main.rs
│   │   └── tests/              # agent.rs (WebSockets + recording injector), uinput.rs (real devices where /dev/uinput is writable)
│   ├── server/                 # Axum dev server: static files + dev TLS + dchat-relay at /nostr
│   │   └── src/
│   │       ├── main.rs         # CLI args, LAN IP detection, QR code banner, /nostr (crates/relay) & /ws routing
│   │       ├── signaling.rs    # Legacy room manager
│   │       └── tls.rs          # rcgen self-signed dev certificate generator
│   └── client/                 # Leptos CSR frontend targeting wasm32-unknown-unknown
│       ├── Trunk.toml          # public_url = "./": relative asset paths (works under any path)
│       ├── index.html          # Trunk entry point, Service Worker registration, PWA links, early beforeinstallprompt capture
│       ├── manifest.webmanifest # Installable app: relative id/start_url/scope, standalone, launch_handler, icons
│       ├── icons/              # icon.svg (source, favicon) + PNGs rendered from it once: 192, 512, maskable 512, apple-touch 180
│       ├── style.css           # Responsive mobile-friendly dark theme
│       ├── service-worker.js   # Caches immutable static assets ONLY (never state)
│       ├── translations.json   # Embedded UI translation table for top 10 global languages
│       └── src/
│           ├── main.rs         # Leptos UI: create/join lobby, room view, member panel, modals
│           ├── agent.rs        # Link to dchat-host: connects on click only, mutual pairing, resume; code and token in RAM
│           ├── i18n.rs         # Strongly typed i18n, browser detection, RTL handling
│           ├── held_media.rs   # Media loaded for viewing: blob: URLs by file id, RAM only, 512 MB budget (least recently viewed released), our own files never
│           ├── ice.rs          # Optional host TURN: GET ./ice-servers (3 s timeout, silent fallback)
│           ├── layout.rs       # Video grid fit: largest 16:9 tiles without scrolling (unit-tested)
│           ├── media.rs        # Capture (mic/camera/screen, chosen device with fallback), device list, speaker choice (setSinkId), per-member <audio>, video attach, speaking meter
│           ├── media_card.rs   # Images, videos, audio and voice messages in the chat: preview choice, auto-load on screen, thumbnail placeholder, fullscreen viewer, one player at a time, voice chaining (unit-tested)
│           ├── mesh.rs         # PeerLink: one RTCPeerConnection per member (chat + file-transfer channels), perfect negotiation, batched ICE, tracks; FileLink (extra chunk-only connections), ChunkRoute, wait_for_room
│           ├── names.rs        # Random session names, name sanitizing, pubkey tags
│           ├── nostr_pool.rs   # Multi-relay pool, fan-out broadcast, deduplication, recipient filtering, per-session relay key certified by the identity, `wake` after a pause (probe, replace silent connections, skip backoff)
│           ├── pwa.rs          # Installed app glue: install offer, Apple hint, app badge, launchQueue links (all feature-detected), iOS detection (unit-tested)
│           ├── qr.rs           # On-the-fly SVG QR code generation
│           ├── recorder.rs     # Voice messages: AudioWorklet mic tap, MP3 in RAM, level meter and waveform, 15-min cap, review, tap/hold/slide gestures (MicPress)
│           ├── remote_input.rs # Viewer capture over a shared screen: pointer (letterbox-aware, sent at once, 4 ms spacing), keys, wheel, heartbeat
│           ├── staging.rs      # Files waiting to be sent (up to 10): previews made from the file (thumbnail, size, length), photo location warning and removal, drop and paste helpers
│           ├── voice_mp3.rs    # Voice MP3: streaming windowed-sinc resampler to 24 kHz, rusty_mp3 encoder at 48 kbps (unit-tested, decoded back)
│           ├── stats.rs        # On-demand tile stats (getStats deltas, requestVideoFrameCallback); no addresses shown, nothing stored
│           ├── session/
│           │   ├── mod.rs      # RoomSession: mesh orchestration, signed gossip, roster, caps, away members and the wake after a pause (link checks), e2e hooks
│           │   ├── admin.rs    # Kick / rotate link: ECDH-sealed AdminRekey, migration to the new room, the rekey handed to members who were away (`RekeyForward`)
│           │   ├── control.rs  # Remote control: requests and grants, input gate to dchat-host, sealed input as a viewer
│           │   ├── extras.rs   # Typing, reactions, edit/delete, ECDH-sealed DMs (waiting in RAM for an away member), @mention detection
│           │   ├── file_links.rs # Extra file links per member: dial/answer over the main link, routes, loss count, idle close, test hooks
│           │   ├── files.rs    # Room-wide file cards, per-requester pulls, upload queue, chunk I/O over every route (window, acknowledgements, resend on link loss, growth), reorder on receipt, pause and resume from the first missing chunk, in-memory downloads folded into one Blob, iOS Save (share sheet)
│           │   ├── history.rs  # The timeline from the chat log: live and synced messages, edits, deletions, reactions, file cards, names; history placed by time
│           │   ├── succession.rs # Admin succession: heir handover (deferred past our own Hello), takeover, Make admin, adminKeyState hook
│           │   ├── sync.rs     # History sync on every link: one pull at a time, summary → asks → wants → batches, paced on the chat channel, test hooks
│           │   ├── quality.rs  # Video presets applied: capture, complete setParameters per sender, codec choice, camera re-sync via jitterBufferTarget
│           │   └── lounge.rs   # Voice lounge: seats, per-link senders, voice/video caps, speaking, controls
│           └── state.rs        # UI types, device choice and picker lists (unit-tested), URL fragment helpers (create room, invite/admin links), relays
├── e2e/                        # Playwright automated multi-peer end-to-end tests
│   ├── playwright.config.js    # Starts the dev server (serves crates/client/dist-e2e) and a recording dchat-host on port 7499
│   └── tests/
│       ├── helpers.js          # createRoom / joinRoom / memberRow helpers shared by specs
│       ├── group_chat.spec.js  # 3-member mesh, fan-out, caps + admin seat, relayed text without a direct link
│       ├── group_voice.spec.js # 3-member lounge: join prompt, mesh audio, mute state, speaking, voice/video caps
│       ├── away_return.spec.js # Phones that switch apps (simulateFreeze/simulateResume): away row and fast return as the same member, left after the grace and joined on a late return, downloads resumed byte-exact (sender or downloader away), card waiting for its sender, DM outbox (delivered, not delivered), away admin and heir keep their roles, kick while away (followed via RekeyForward, kicked member removed on return), voice back with audio
│       ├── admin_succession.spec.js # Last admin gone: the longest-present member takes over after 15 s (kill or leave), an admin reload changes nothing, the chain goes on, Make admin (confirm, can't be undone, link survives reload), heir leaving or reloading, seniority across a new link, no takeover while alone
│       ├── group_admin.spec.js # Kick (removed screen, others migrate with chat), rotate link (voice rejoin, old invite stranded)
│       ├── group_history.spec.js # Always-on history: edits/deletions/reactions/file cards and departed names for late joiners, A→B→C→D hand-on, reconnect catch-up without duplicates, kept through a kick (authors still edit, files still download), one pull per newcomer, loading line with live chat, old hist=1 links, reload, cross-room replay refused
│       ├── group_extras.spec.js  # Typing, reactions, edit/delete, direct + relayed DMs (relay can't read), @mention highlight
│       ├── p2p_chat.spec.js    # 2-member room, E2EE message exchange, reload wipe, fragment params
│       ├── host_turn.spec.js   # Host-offered TURN (./ice-servers) used; 404 falls back to STUN; no room data sent
│       ├── relay_choice.spec.js# Relay choice at room creation: public default, my relay only, my relay + public
│       ├── relay_reconnect.spec.js # Dropped relay connection reconnects; newcomers still reach the member
│       ├── link_renegotiation.spec.js # With every relay cut after linking, voice and video still negotiate over the link
│       ├── protocol_version.spec.js # Different protocol versions never link; the older member gets a reload banner
│       ├── room_password.spec.js # Password rooms: link + password, wrong password finds nobody, rekey keeps the password
│       ├── remote_control.spec.js # dchat-host pairing, request/allow, clicks through letterboxing, one holder, others' raw input dropped, release on shortcut/revoke/link loss, prompts in fullscreen, window shares not offered, game mode (pointer lock, relative moves, video left to the sharer's preset), controllers (P1/P2 per member, neutral + unplug on revoke, kept alongside mouse/keyboard), clicks through an open stats panel, no move after the release shortcut, moves ≥ 4 ms apart with the last one delivered, source screen size sent to dchat-host under every preset
│       ├── privacy.spec.js     # Sealed handshakes vs the room key (identity inside, per-session relay key, a new one after a rotate), STUN fallback, hideip through a real TURN (node-turn)
│       ├── audio_call.spec.js  # 2-member lounge audio, mic/speaker mute, leave (replaceTrack null) and rejoin
│       ├── video_call.spec.js  # Camera tiles, camera flip keeps the mic (and HD's 1280×720), camera off, grid teardown
│       ├── screen_share.spec.js# Screen share, switch to camera on the same sender, stop
│       ├── video_quality.spec.js # Presets: sender parameters per link, live switch, caps, stale state cleared, own msid + camera re-sync, lifetime across rekey, stats panel, glass-to-glass latency
│       ├── media_preview.spec.js # Pictures load by themselves once on screen (history only when scrolled to), viewer, Download from RAM (no new request), 16 MB tap-to-load, video kept through a reaction, SVG/fake pictures stay plain, budget back to thumbnail, thumbnail after the sender left, Remove location
│       ├── voice_message.spec.js # Record, review, send, play; hold/slide-cancel/slide-lock; discard sends nothing; cap; lounge mic muted while recording; chained playback; review kept across a rekey
│       ├── attach_drop.spec.js # Drag and drop (overlay, several files, one card each, caption on the first), 10-file cap, text drags and stray drops, pasted screenshots, no drop while recording
│       ├── file_sharing.spec.js# Room-wide file cards: parallel pulls, withdraw, cancel and retry, upload queue, unreachable sender, sender leaving; file links (chunks spread and reordered byte-exact, lost link resent, growth and the settings cap)
│       ├── ios_save.spec.js    # iPhone user agent + stubbed share sheet: no download unasked, 💾 Save shares exact bytes, closed sheet keeps Save, download fallback, survives a rotated link, 20 MB byte-exact
│       ├── audio_settings.spec.js # Mic processing checkboxes, live track swap in voice, carry-over, reload reset
│       ├── devices.spec.js     # Mic/speaker/camera pickers (two fake cameras): live mic swap keeping the mute, camera switch keeping the preset, 🔄 to the next camera, setSinkId, missing device fallback, RAM only
│       ├── pwa.spec.js         # Manifest/icons cached, Join with a link (any site, bare fragment, key never requested), launchQueue (lobby joins, live room asks), install offer, Apple hint, app badge
│       └── i18n.spec.js        # UI localization, dynamic switching, Arabic RTL, zero persistence
├── .github/workflows/
│   ├── build.yml               # Reusable: Rust + Worker tests, release build, no-hooks check, "site" artifact
│   ├── pages.yml               # Deploy the site to GitHub Pages on push to main
│   ├── cloudflare.yml          # Deploy to Cloudflare Workers (skips until CLOUDFLARE_* secrets exist)
│   ├── relay-image.yml         # Publish ghcr.io/<owner>/dchat-relay when the relay changes
│   ├── host-agent.yml          # dchat-host tests and release build on Linux and Windows
│   └── host-agent-release.yml  # Tag host-vX.Y.Z: static Linux (musl) and Windows downloads, README, SHA256SUMS, GitHub release
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
  - Direct disk streaming via File System Access API (`showSaveFilePicker`) with fallback to in-memory Blob download. The fallback merges chunks into one Blob every 8 MB (`FOLD_BYTES`), so finishing never holds the file twice.
  - iOS (every iPhone/iPad browser, `pwa::is_ios`): a finished in-memory download is never handed over unasked (iOS would open it in another app and suspend dchat). It waits as `ReadyToSave` in the app-level `ready_files` map (kept across rekeys, cleared on the removed screen) until the person taps 💾 Save, which opens the share sheet (`navigator.share({ files })`, `<a download>` where files can't be shared). A closed sheet keeps the button; a successful share frees the bytes.
  - Staging attachment chip, interactive file cards in chat, progress bars, real-time speed calculation, and cancel controls (a Decline button existed until Phase 12: in a room it only hid the buttons locally, and couldn't be undone).
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
  - Device pickers (Devices section, first in ⚙️ settings): microphone, speaker and camera as a RAM-only `DeviceChoice` pushed into every session (so it survives rekeys). Capture uses `deviceId: { exact }`; a device that is gone (`OverconstrainedError` / `NotFoundError`) is retried once as the system default, with a toast, and the choice is cleared. A new mic goes through the same latest-wins swap as the processing switches; a new camera through the 🔄 recapture (hint before attach, sender parameters re-applied), and 🔄 itself moves to the next listed camera when one is chosen. Speakers use `setSinkId` on every `audio.remote-audio` (hidden where unsupported, e.g. Safari; Firefox's `selectAudioOutput` chooser where offered).
- **Phase 8: Discord-like Group Rooms over a Serverless Mesh (Completed)**
  - **8a Mesh core (Completed)**: create/join lobby with session nicknames; full-mesh `PeerLink`s with addressed signaling and perfect negotiation; signed `RoomEnvelope` gossip with relay to members lacking a direct link; roster with mutual-link reachability and `direct` / `via X` / `connecting` link states; per-room member cap (`&max=`, default 25, `0` = unlimited) with deterministic latest-joiner eviction and an admin seat; admin link (`adm`/`admsk`) vs invite link; optional TURN in the fragment; join/leave notices.
  - **8b Voice lounge (Completed)**: drop-in lounge replaces the ring flow (`CallInvite`/`CallAccepted` gone); signed `VoiceState` gossip (seat time, mic, video kind); per-room voice/video caps (`&maxa=`, `&maxv=`) with the same latest-loses rule (admins not exempt); one audio + one video sender per link (`addTrack` once, then `replaceTrack`, `None` to stop: no renegotiation on toggles); camera/screen as one video source with front/rear flip; per-member hidden `<audio class="remote-audio">`; Web Audio speaking meter; video grid with fullscreen; "X joined voice" prompt; audio settings carry over between joins.
  - **8c Group file sharing (Completed)**: `FileOffer` card gossiped to the room; `FileRequest`/`FileQueued`/`FileCancel{to}` are direct-only envelopes (`RoomBody::recipient`), applied only when received straight from their author and never relayed; per-link binary `file-transfer` channel with backpressure; chunks accepted only from the offer's author over its direct link and in order (since Phase 12 also over the author's file links, put back in order); `UploadQueue` (max 2 concurrent, FIFO, unit-tested); cancel, withdraw (room-wide); transfers stop on link loss, offers marked unavailable when the author leaves the present set. Merged to main after 8c.
  - **8d Moderation & history (Completed)**: `AdminRekey { kicked, grants }` is gossiped and signed by an admin session (verified via its Hello admin proof); each grant (new room ID + key) is sealed with ECDH between the admin's and the member's session keys (`NostrBurnerKey::shared_key`, `SealedGrant`), so relays and the kicked member can't read it; recipients wait 1.5 s (relay flush), leave, rewrite the fragment and start a new session (chat kept, voice auto-rejoined, join notices muted for 5 s); the kicked member gets a removed screen. History (replaced by Phase 13): `Chat.shareable` from the author's own link, `HistoryBuffer` (200, signed originals) served to up to 2 neighbors on request in batches of 40 together with the authors' archived Hellos (names only, never roster); inserted by timestamp. Links stuck in `disconnected` for 10 s now count as lost so killed tabs leave promptly.
  - **8e Chat extras (Completed)**: `Typing` (throttled 3 s, expires after 4.5 s); `Reaction{target, emoji, on}` limited to `REACTIONS`, latest toggle per member wins (`Reactions`, unit-tested); `Edit`/`Delete` honored only from the original author (since Phase 13 the chat log holds both, and late joiners get the latest edit); `Dm{sealed}` sealed with `seal_json` (ECDH session keys), sent only over the direct link when there is one, otherwise gossiped; it names no recipient (protocol v3): every member tries to open it, only the recipient can, and the recipient never relays it; `mentions()` with word boundaries on both sides; chime via Web Audio; `(n)` title badge while hidden; DM threads end when the peer leaves.

- **Phase 9: Remote Control (Completed)**
  - **9a Desktop control on Linux (Completed)**: protocol v4 (`ControlStatus`/`ControlRequest`/`ControlGrant`/`ControlRelease`, sealed input packets, `DomCode`); `dchat-host` (loopback WebSocket, mutual pairing, engine with release-on-exit and watchdog, uinput backend, X11/XWayland monitor layout); tab ↔ app link; permission prompts that follow fullscreen; desktop-mode mouse and keyboard with letterbox-aware positions; E2E against a recording dchat-host.
  - **9b Windows and game mode (Completed)**: `SendInput` backend (scan codes, `VIRTUALDESK` absolute positions, per-monitor DPI awareness, administrator note), Windows monitor layout, `win_input` records unit-tested on every platform, CI on Windows; viewer game mode (pointer lock with `unadjustedMovement`, relative moves, keyboard lock in fullscreen, losing the lock ends control). Since Phase 10, `Mode` no longer changes the shared video: the sharer's preset decides.
  - **9c Controllers (Completed)**: viewers send their first "standard" gamepad (`PadPoller`, `pad_state_from`, `PadSampler` heartbeat) while they hold a slot; `ControlStatus.controllers` advertises how many virtual pads the app can create (`ControlState::set_pad_slots`); dchat-host plugs a virtual Xbox 360 pad per slot (uinput copy of xpad `045e:028e` on Linux, ViGEm on Windows with an install hint when the driver is missing), neutralizes a pad after 0.5 s without updates, and unplugs it on revoke.
  - **9d Releases and polish (Completed)**: global stop shortcut (`global-hotkey`: Windows message loop, X11; Wayland reported unavailable, the GlobalShortcuts portal is left for later); panic hook and SIGHUP / Windows console-close, logoff and shutdown all release held input; release workflow (`host-v*` tags: static musl and MSVC `+crt-static` builds, packaged with the udev rule and README, `SHA256SUMS`, optional `DCHAT_HOST_ORIGINS` repository variable).

- **Phase 10: Low-latency video with quality presets (Completed)**
  - Sharer-wide presets (screen: Fastest, Smooth, Balanced, Sharp, Text; camera: Smooth 60, Balanced, HD, Full HD, Data saver; Balanced by default) in the settings modal and a ▾ menu next to the camera and screen buttons; RAM only, kept across voice rejoins and rekeys (audio settings now also survive a rekey).
  - Each preset sets capture (full `applyConstraints` set), `maxBitrate` (lifts Chrome's 2.5 Mbps default), `maxFramerate`, `scaleResolutionDownBy`, `degradationPreference`, `contentHint` and the codec (hardware H.265/H.264 for motion, AV1/VP9 for detail when the browser reports them smooth). Game mode no longer touches video.
  - Video leaves the voice's lip-sync group; viewers re-sync camera tiles with `jitterBufferTarget`. Pointer moves are sent at once (4 ms spacing) and paced toward dchat-host. ⓘ stats per tile (RTT, buffer, decode, to-screen, encode, send delay, limiting factor).
  - Future (not built): encode once for many viewers; Chrome's Encoded Source is the preferred route, so the preset table stays engine-neutral and each link keeps one video sender with one encoding.

- **Phase 11: Installable app (PWA) (Completed)**
  - A native client was considered and set aside: a truly native build means a second client plus a media engine, a webview shell lacks WebRTC on Linux, and no web app can inject OS input. `dchat-host` stays the remote-control helper, unchanged.
  - Manifest (standalone, relative scope, `launch_handler`), icons (SVG source + PNGs, maskable and Apple), precached by the Service Worker. UI only: no `PROTOCOL_VERSION` change.
  - **Join with a link** in the lobby (any dchat site's link or a bare fragment, `FragmentParams::from_link`); room links launched into an open app window go through `launchQueue` and ask before leaving a live room.
  - **📲 Install app** from Chromium's `beforeinstallprompt` (captured early in `index.html`), an Add to Home Screen / Add to Dock hint on Apple WebKit, both on the create screen only and hidden when installed; the app badge mirrors the `(n)` @mention title badge (`setAppBadge`).

- **Phase 12: Parallel file links (Experiment)**
  - Why: one SCTP association's congestion window limits a data channel to a few MB/s over internet round trips, whatever the line speed. Separate `RTCPeerConnection`s have separate windows; extra data channels on one connection do not. Measured over an emulated 40 ms, 30 MB/s link (Firefox, 200 MB): one connection 3–6 MB/s, four 5–9 MB/s.
  - Protocol v5: `FileLinkSignal`, acknowledgements, reorder window and resend-on-loss (see the invariants above). Chunks go to the emptiest open route (main link first, round-robin on ties).
  - Growth (`LinkGrowth`): the upload starts on the main link plus any file links kept open from an earlier upload; once the main link's ramp-up settles it adds one link, judges it 3 s after it opens on the rate the connections take from the send queues (finer than acknowledgements), keeps it if the rate rose by more than 10 % (with a second look before giving up), and stops at the cap or when not worth it (under 4 MB or 5 s left). A link that did not help is set aside (kept open, no new chunks) and can be taken back by the next upload.
  - UI: ⚙️ Settings → Files picks the cap (1, 2, 4, 8; RAM only, kept across rekeys). It limits both directions and applies at once: our uploads use at most that many routes (a running upload restarts its growth), and links members dialed to us past it close (their sender resends) or are refused. Both cards show "· N connections", counted the same way on each side: the connections that carried a chunk in the last 2 s. The shown speed is a 5 s average refreshed once a second; a finished download shows its size, time (from the first chunk) and average speed, and offers Download again unless the offer was withdrawn. The console logs every growth decision and each finished download.
  - Progress updates (`Downloading`, `Sharing` with new numbers) don't bump the message's `rev`: the row stays, and the card reads its live status through a memo, so its buttons are never rebuilt under the pointer (they flickered on hover and could lose clicks).
  - Test hooks: `forceFileLinks(n)`, `fileLinkChunks()`, `openFileLinks()`, `dropFileLink()`, `discardFileLinkChunks(on)`.
  - Testing notes: on localhost a new connection's first burst overflows the receiver's 208 KB UDP buffer and stalls ~2.5 s, so localhost numbers don't show the gain. Through a userspace delay+rate emulator, Playwright's headless Chrome stalls even on one bare data channel while Firefox doesn't, so WAN emulation was done in Firefox.

- **Phase 13: Always-on history (Completed)**
  - Protocol v6. The opt-in (`#history-checkbox`, `&hist=1`, `Chat.shareable`, `HistoryBuffer`, the 🕒 badge, the "aren't visible" notice) is gone; old links carrying `hist=1` still work and new rooms never write it.
  - No CRDT library: the log is a set of signed envelopes with order-independent rules (see the History invariant), so signature attribution and author-only edits/deletions keep working, at no wasm cost. automerge/yrs would apply anyone's changes.
  - Room messages are signed over the room's `adm` (cross-room replay refused). Identity carried across rekeys; relay events signed by a fresh relay key the identity certifies (`RelayFrame`).
  - Sync on every new link, both ways, one pull at a time; `Sync{Summary,Ask,Diff,Want,Batch}` direct-only. A member whose links dropped catches up on what it missed. "Loading earlier messages…" (`.history-loading`) while a pull fetches; chat stays usable. The "earlier messages" line appears once rows from before we joined are inserted.
  - Live and synced envelopes go through the same log (`history.rs`): rows are built from `ChatLog::view` (latest edit, reaction tally, withdrawal), history placed by time in one pass (`state::merge_by_time`), mentions highlighted without chime or counter. Edits/deletions of rows the log no longer holds (too large, older than the horizon) still apply to the row when they come from its author.
  - After a rekey, file cards on screen are known again from the log (`restore_files`), and our own offered files stay downloadable (`SessionCarry.shared_files`); asking for a file its sender no longer has gets a `FileCancel` back.
  - Test hooks: `unblockPeer(pk)`, `throttleSync(ms)`, `syncStats()`, `exportEnvelope(text)`, `injectEnvelope(pk, json)`.
  - Known limits: a full 32 MB first sync costs ~100k signature checks (seconds of CPU, newest first, UI usable); members whose clock runs > 5 min fast talk live but stay out of history; author-chosen ids can be squatted (first held wins) and XOR digests aren't collision-resistant against crafted ids: both cost bounded re-syncs, never forged content. Members at the budget limit may disagree on their oldest entries after a deletion frees space.

- **Phase 14: Admin succession (Completed)**
  - Protocol v7: `AdminHandover { promote, sealed }` (gossiped, no recipient field, `HandoverContent { admsk, ranking }`), `protocol::succession` (see the invariant above).
  - The heir is the admin's longest-present member as its own tab saw it, inherited down the chain with the ranking; seniority survives a rekey (`SessionCarry.first_seen`, same identities). An admin reload within 15 s changes nothing; the alone guard keeps a heir whose own network dropped from promoting itself.
  - UI: **Make admin** next to Kick (confirm: can't be undone), 🔑 Copy Admin Link follows `am_admin` (shown to any admin whose link holds the secret), *"Name is now an admin"* notice, toast telling a new admin to share 🔗 Copy Link rather than the address bar.
  - Test hook: `adminKeyState()` → `"active" | "dormant" | "none"` (never the secret).
  - Known limits: if the admin and the heir vanish within the same 15 s, nobody takes over until someone opens an admin link; a partitioned heir that still sees others can become a second admin (harmless: several admins are allowed); a former or kicked heir still knows `admsk` (rotating the admin keypair on kick would fix it); concurrent rekeys by two admins can split the room (more likely with more admins).

- **Phase 16: Away and return (Completed)**
  - Why: on Android and iOS, switching apps freezes the tab; its links died after 10 s and members saw it leave (files gone for good, DMs ended, a new admin after 15 s, stranded by any rekey, 30 s+ to reconnect). The tab itself survives with its identity in RAM, so the fix is to keep its place, not to persist anything.
  - Protocol v9: `protocol::presence` (`AWAY_GRACE_MS`), `FileRequest.from_chunk`, `RoomBody::LinkCheck`, `SignalPayload::RekeyForward`; succession counts away members.
  - Away is inferred (no away/back message: members learn nothing new); the 5-minute grace, admin and heir rules, caps, DM outbox, resume and rekey forwarding: see the invariant above. UI: dimmed `away` row, *Available when <name> is back*, *Paused: waiting for <name>*, DM *Waiting* / *Not delivered*.
  - Test hooks: `simulateFreeze()`, `simulateResume(ms)`, `awayGraceMs(ms)`, `awayMembers()`. Specs that kill a tab (`context.close()`, no `Leave`) and expect it gone shorten the grace with `awayGraceMs`.
  - Known limits: a crashed tab (or a closed context) is listed as away for 5 minutes, and an admin's crash leaves the room without a working admin for that long; a reload or a tab the OS discards is a new member (nothing is stored); partial downloads are not carried across a rekey; if the admin leaves while the heir is away and the heir doesn't return within the grace, nobody takes over; an upload whose file can no longer be read after the pause ends without telling the downloader (which waits until the sender leaves).

- **Phase 15: Media previews & voice messages (Completed)**
  - Protocol v8: `FileOffer.media` (`protocol::media`), validated alike by every member (`RoomBody::is_well_formed`, the chat log's `Ignored::Malformed`).
  - Pictures, GIFs, videos and audio show in the chat (`media_card.rs`): the sender's thumbnail until loaded, auto-load ≤ 16 MB once on screen, a tap up to 200 MB, plain download above; fullscreen viewer; one player at a time. Download saves the copy in RAM (`HeldMedia`, 512 MB budget, kept across rekeys). The sender's own card plays from its own file. Media cards show the same transfer numbers as file cards. A loaded file that fails to open gets up to `MAX_REFRESHES` fresh `blob:` addresses (Chrome's bare "MEDIA_ELEMENT_ERROR: Format error" is an unreadable address, often fine moments later), then keeps its card with the browser's reason; a held copy that can no longer be read is released, so Download pulls it again.
  - Voice messages (`recorder.rs`): 🎤 instead of Send while there is nothing to send; tap for hands-free, hold with slide-to-cancel and slide-up-to-lock; always a review (play, seek, ➤ / 🗑); 15-minute cap; MP3 in every browser, never `MediaRecorder` (Firefox's WebM didn't play on Brave for Android): `TAP_JS` AudioWorklet → `voice_mp3` (windowed-sinc resampling to 24 kHz, `rusty_mp3` at 48 kbps, encoded as the samples arrive, Info header first); waveform from each block's level; the lounge mic is muted while recording. Bubbles: waveform seek, 1×/1.5×/2×, consecutive voice messages chain.
  - Up to 10 staged files (`staging.rs`): 📎 picks several, drop anywhere on the room (overlay; drops never navigate the tab), paste files or screenshots (`paste-<date>.png`); one card per file, caption on the first. JPEG location warning and **Remove location** (EXIF GPS zeroed in place, XMP positions dropped).
  - Reactions no longer bump a row's `rev` (the row reads them through a memo).
  - Posting alone: chat, files and voice messages are accepted as soon as the tab is in the room (`ConnectionStatus::in_room`), not only once a link is open; they wait in the chat log and reach whoever joins through history sync (files stay pullable from the author while present).
  - Test hooks: `fileRequestsSent()` on `window.__dchat`; `window.__dchatMedia` with `mediaBudget(bytes)`, `recordCapMs(ms)`, `heldMedia()`.
  - Known limits: no mirrors (a file is gone with its sender, its thumbnail stays); location detection is JPEG only; a media file a browser fails to open keeps its card with the browser's reason and Download from RAM; synthetic tests can't cover folder drops or the real clipboard.

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
- [ ] `e2e-hooks` code (`window.__dchat.selfPubkey/blockPeer/unblockPeer/throttleUploads/adminKeyState/fileRequestsSent`, the history hooks `throttleSync/syncStats/exportEnvelope/injectEnvelope`, the file-link hooks `forceFileLinks/fileLinkChunks/openFileLinks/dropFileLink/discardFileLinkChunks`, the away hooks `simulateFreeze/simulateResume/awayGraceMs/awayMembers`, `window.__dchatMedia.mediaBudget/recordCapMs/heldMedia`, and reading `window.__dchatProtocolVersion`) stays behind `#[cfg(feature = "e2e-hooks")]` and out of `make build-client` output.
- [ ] Offers, answers, ICE and forwarded rekeys (`RekeyForward`) reach the relays only as `RelaySignal::Sealed` (never readable with the room key alone); `RekeyForward` is honored only through the relays, only if the envelope verifies with our own `adm` and comes from a roster admin.
- [ ] Away members cost nothing on disk: their place, waiting DMs (sealed to them before queuing) and paused downloads live in RAM only and end with the grace; `LinkCheck` is direct-only; nothing about a member survives a reload.
- [ ] No third-party server is contacted by default when the room or host provides one (Google STUN only via `plan_ice`'s fallback); `hideip` rooms gather relay candidates only.
- [ ] Wire or shared-rule changes bump `PROTOCOL_VERSION` and record the new fingerprint; signals from other versions are reported, never parsed or linked.
- [ ] `LinkSignal` is applied only from the link's own remote (direct-only), and only for offers, answers and ICE addressed to us (never presence, departures or forwarded rekeys).
- [ ] Direct-only room messages (`RoomBody::recipient()` is `Some`) are applied only when `to` is us and the envelope came straight from its author; they are never relayed.
- [ ] File chunks are accepted only from the offer's author over that author's own connections (main link or their file links), and written strictly in order: early ones wait in the reorder window, stale ones and anything past the window are dropped, an inconsistent header aborts. Acknowledgements are applied only to our own offer's upload to the member whose connection carried them.
- [ ] File links are negotiated only over the open main link (`FileLinkSignal`, direct-only), with the room's ICE configuration; at most 7 per dialer; they close with the main link and on leave.
- [ ] On iOS a finished download leaves dchat only through the person's 💾 Save tap (share sheet); its bytes stay in RAM (`ready_files`) and are dropped once shared or when the tab leaves the room.
- [ ] `AdminRekey` is honored only from a member whose Hello carried a valid admin proof; grants are sealed per recipient (never the room key in clear).
- [ ] `AdminHandover` is honored only from a roster admin and only by non-admins; the opened secret must be `adm`'s; a newer heir grant that isn't ours drops a dormant copy; a heir takes over only after `ADMIN_ABSENT_MS` without an admin while another member is present; a dormant secret never enters the link, logs or `Debug` output.
- [ ] Remote control: nothing is granted without the sharer's click; input is accepted only from current holders, over their own direct link, sealed with the input AAD; dchat-host stays loopback-only with Host/Origin checks and mutual pairing, releases held input on every exit path, and logs no input.
- [ ] DM plaintext is only ever sealed with `seal_json` to the recipient's session key; DMs carry no recipient field; the recipient never relays a DM; no DM text in logs.
- [ ] A room password never appears in the link, logs, messages or storage: only its salt (`pw`) is in the link, the input is cleared after stretching, and only the stretched value stays in RAM.
- [ ] Text inputs and lobby forms keep `autocomplete="off"`, except the create form's password box, which exists only while **Protect with a password** (`#password-checkbox`) is ticked and uses `autocomplete="new-password"` (Chromium ignores "off" on password boxes and would fill a saved password into every new room); message boxes follow the spell-check setting.
- [ ] Edits/deletes are applied only when the envelope author equals the original message author.
- [ ] The chat log holds and serves only its own kinds (chat, file cards, edits, withdrawals, reactions, deletions, Hellos), never DMs, notices or direct-only bodies; `Sync*` bodies are direct-only, batches are accepted only from the active pull peer and only for ids asked for (or Hellos), and every synced envelope is verified with our own `adm` before it is merged; Hellos from history only label names, never join the roster.
- [ ] Room envelopes are signed and verified over the room's `adm`; relay frames are accepted only when their relay key is certified by the identity they claim.
- [ ] Video presets stay local (never in `RoomBody` or the wire fingerprint); `setParameters` writes complete state; `applyConstraints` always gets the full set and never `frameRate.min`.
- [ ] The stats overlay never shows addresses, ports or candidates, and stores nothing.
- [ ] Input to dchat-host goes through the `AgentPacer`; nothing reaches the app after `ReleaseAll` (the viewer's trailing move flush is cancelled first).
- [ ] Media previews: only `preview_type`'s allow-list (never SVG) is shown inline, from a Blob built with the allow-listed type; thumbnails come only from a valid `MediaInfo` (WebP/JPEG `data:` URL in `<img>`); `FileOffer.media` is validated by `RoomBody::is_well_formed` before showing or logging; loaded media lives only in `HeldMedia` (RAM, budgeted, cleared on the removed screen) and object URLs are revoked when released.
- [ ] Voice recordings stay in RAM (no storage, no automatic download), are sent only from the review (➤), and the microphone track is stopped when recording ends or is cancelled; the lounge mic mute is restored as it was.
- [ ] Dropped and pasted files only join the staged list (≤ 10); a drop anywhere in the room is `preventDefault`ed so the browser never navigates away from the room.
- [ ] Links from outside the page (paste box, `launchQueue`) use only their fragment, via `FragmentParams::from_link` and `state::join_link`; leaving a live room for one asks first; the manifest has no `protocol_handlers`, and the Service Worker precaches only fixed-name app files (manifest, icons).
