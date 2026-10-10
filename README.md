# 🔒 dchat — Ephemeral Zero-Knowledge P2P Chat

> **Disposable, browser-cached peer-to-peer chat built in pure Rust and WebAssembly, bypassing Emscripten entirely.**

All chat state, keys, and message history reside strictly in WebAssembly linear memory, never on disk. Refreshing or closing the page wipes that tab's copy forever; the room's conversation lives only in the memory of the members still in it, and is gone for good once the last one leaves.

---

## Key Features & Security Guarantees

### Rooms & Chat

* **Group Rooms over a P2P Mesh**: One link opens a room for a small group (25 members by default; the creator can also limit how many join voice and turn on video). Every member connects directly to every other member: there is no media or message server, only Nostr relays for the initial handshake. Share the invite as a link or a QR code; members without a direct link still get text, relayed through a mutual member.
* **Signed Messages**: Every room message is signed by its author's session key and passed on to members without a direct link, so nobody can alter it or speak for someone else. Names live for the session only and are shown with a short key tag.
* **Always-On History**: Whoever joins sees the room's conversation as members see it: messages with their latest edits, deletions, reactions, file cards and the names of members who left. Members sync what they hold on every new connection, so someone whose connection dropped for a while catches up too. Kept in RAM only (up to 32 MB per member), carried through a kick or a new link, gone when the last member leaves.
* **Chat Extras**: Typing indicator, emoji reactions, editing and deleting your own messages, private messages sealed end-to-end between two members, and @mentions with a highlight, a chime and a count in the tab title (and on the app icon when installed).
* **Moderation**: Admins can kick a member or move everyone to a new link (new room ID and key, sealed to each remaining member). The admin link is separate from the invite link, and an admin always gets a seat in a full room.
* **Admin Succession**: When the room's last admin leaves, the member who has been there longest becomes admin after 15 seconds, and admins can make another member an admin with **Make admin**.
* **10 Languages**: English, Chinese, Hindi, Spanish, French, Arabic (with a right-to-left layout), Bengali, Portuguese, Russian and German, picked from the browser's language with English as the fallback and switchable from the header. The translation table is built into the Wasm (`translations.json` via `include_str!`); the choice stays in RAM.

### Voice, Video & Screen Sharing

* **Voice Lounge**: Discord-style drop-in voice and video with per-person mic, speaker, camera (front/rear flip on mobile) and screen-share toggles, a video grid with fullscreen, a speaking indicator and a *"X joined voice"* prompt. Audio and video are encrypted between members (DTLS-SRTP).
* **Audio Settings & Devices**: ⚙️ Settings turn noise cancellation, echo cancellation and auto gain control on or off (all on by default), applied live without dropping the call, and pick the microphone, speaker and camera. Switches the browser doesn't support are disabled with a hint. Kept in RAM only.
* **Low-Latency Video Presets**: Pick how your camera and screen share look to everyone: from **Fastest** (720p, 60 fps, lowest latency) to **Text** (full resolution, 15 fps, crisp and light). Video doesn't wait for voice lip sync, and an ⓘ panel on each tile shows what the connection is doing.
* **Remote Control**: While you share your screen, the members you allow can control your mouse and keyboard (one at a time, desktop or game mode), TeamViewer-style, and play with game controllers (up to four). A small companion app for Linux and Windows, `dchat-host`, does the input on your computer; nobody gets control without your click. Downloads are published per release with checksums.

### Files

* **Encrypted P2P File Sharing**: Files are posted to the room as cards, and each member pulls the file straight from the sender over their own direct link, in 64 KB chunks each sealed with ChaCha20-Poly1305 (header authenticated too). The sender uploads to two members at once and queues the rest; cards show progress and speed, with cancel and withdraw. Downloads stream to disk where the browser allows it (File System Access API) or are saved from memory; on iPhone and iPad, 💾 Save opens the share sheet. Files are never stored on a server, and a transfer stops if the sender leaves.
* **Parallel File Connections (experiment)**: Uploads add extra WebRTC connections to the downloader while each one raises the speed, up to a cap set in ⚙️ Settings, for internet links where one connection can't fill the line.

### Privacy & Security

* **Zero Persistence**: Strictly **no** `localStorage`, `sessionStorage`, `indexedDB`, or cookies. Everything is held in volatile RAM.
* **Zero-Knowledge URL Keys**: The room ID and 256-bit symmetric encryption key reside in the URL fragment (`#room=<id>&key=<secret>`). URL hash fragments are never sent to any server over HTTP or WebSocket handshakes. Rooms can also require a password, so a leaked link alone doesn't open them.
* **Dual-Layer E2EE**: In addition to standard WebRTC DTLS, messages and signaling envelopes are encrypted with **ChaCha20-Poly1305** using the URL fragment key. Relays are completely blind to message contents.
* **Metadata Protection**: WebRTC handshakes (which carry IP addresses) are sealed to their recipient, Google's STUN server is only a fallback, and rooms can hide members' IP addresses from each other by connecting through TURN. See [`docs/PRIVACY.md`](./docs/PRIVACY.md) for who sees what.
* **Ephemeral Burner Keypairs**: Generates fresh in-memory secp256k1 keypairs in each tab: a member identity that signs room messages (kept when an admin moves the room, so you stay the author of your messages) and a relay key, new every session, that signs the BIP-340 Nostr events, so relays can't link a moved room to the old one. All of them are discarded on exit.
* **Instant Destruction**: Reloading the page or closing the tab wipes linear memory, destroys the WebRTC connections, and permanently erases this tab's copy of the conversation. Members still in the room keep theirs (and hand it to whoever joins) until the last one leaves.

### Signaling & Hosting

* **Decentralized Nostr Signaling**: Replaces proprietary signaling servers with public, open Nostr relays using NIP-16 Ephemeral Events (Kind 20001, dropped upon dispatch, zero disk storage). Room topics are hashed (SHA-256), so relays never see room IDs; ICE candidates are batched to stay under relay rate limits; perfect negotiation settles who calls whom. Each room's relays travel in its link (`&relays=`), and a header badge shows how many are connected.
* **Multi-Relay Pool Resilience**: Broadcasts and subscribes across a concurrent relay pool (`wss://relay.damus.io`, `wss://nos.lol`, `wss://relay.primal.net`) with automatic event deduplication and reconnection.
* **Serverless Architecture**: 100% static client deployable to GitHub Pages, Cloudflare Pages, Netlify, or IPFS. No backend required!
* **Installable App**: Install dchat as an app (PWA) on desktop, Android, iPhone and iPad: its own window and icon, room links that open in the app, and **Join with a link** in the lobby. Still nothing stored.
* **Service Worker Caching**: Caches immutable application shell assets (`.wasm`, `.js`, `.css`) for instant loading, while never storing session or user data.

### Development

* **Pure Rust to WebAssembly**: Built with **Leptos (CSR)** targeting `wasm32-unknown-unknown` without Emscripten.
* **Instant Mobile Testing**: Built-in dev HTTPS server auto-generates TLS certificates, includes an in-memory Nostr relay, and displays an ASCII QR code in the terminal for instant phone pairing on local Wi-Fi.
* **Automated Tests**: Rust unit and integration tests, plus a Playwright suite that runs several browser members against each other offline (see [Automated Verification & Testing](#automated-verification--testing)).

---

## Group Rooms

1. **Create**: open the app, pick a name for this session and a member limit, then tap **✨ Create Room**. The room ID, the room key and an admin key are generated in your browser and placed in the URL fragment.
2. **Invite**: tap **🔗 Copy Link** or **📱 Scan QR**. Both share the *invite* link. Only admins also see **🔑 Copy Admin Link**, which adds the admin secret (`admsk`); share it only with co-moderators.
3. **Join**: whoever opens the invite picks a name and taps **🚪 Enter Room**. Names live in memory only and are shown with a 4-character key tag (`Ana · 3f2a`), so two people with the same name stay distinct. The tag tells people apart; it is not proof of identity.
4. **Member list**: each member shows how you reach them: `direct`, `via <name>` (no direct link, text is relayed through that member), or `connecting…`. The admin carries an `ADMIN` badge.
5. **Moderation (admin link only)**: next to each member, **Kick** moves everyone else to a new room ID and key; the kicked member sees *You were removed from the room*. **🔄 New Link** does the same without removing anyone, so the old invite stops working. The conversation is kept (whoever joins the new room gets it too), you stay the author of your earlier messages, files you shared stay downloadable, and anyone in voice is reconnected automatically. Share the new invite (**🔗 Copy Link**) with anyone who was offline during the move: they can't follow on their own. Kicking needs an admin online, and admins can't kick each other. **Make admin** (next to Kick) gives a member the admin secret at once: their link gains it, so they become an admin like the creator. It can't be undone, since the secret can't be taken back and admins can't kick each other, so the app asks first.
6. **When the last admin leaves**: while an admin is in the room, the member who has been there longest (as the admin's tab saw it) quietly holds a copy of the admin secret, in memory only and unseen by others. If no admin is in the room for 15 seconds while someone else is, that member takes over: the secret goes into their link, they get the `ADMIN` badge, and everyone sees *"Name is now an admin"*. They then pick the next one in line the same way. An admin who reloads is back within those 15 seconds, so nothing changes. If the admin and that member drop out within the same 15 seconds, the room has no admin until someone opens an admin link again. Kicking the member who holds the copy is allowed; they still know the admin secret, but get no key to the new room.
7. **Member limit**: when a room is full, the member who joined last sees *Room is full*. Everyone applies the same rule (join time, then key), so all members agree on who stays. An admin session always gets a seat and bumps the latest non-admin.

Every room message is signed with its author's session key, so a member relaying it cannot alter it or forge messages from someone else.

**Room passwords**: optionally set a password when creating a room: tick **Protect with a password** first (it is off by default, and the box asks for a new password, so the browser never fills in one it saved). Joining then needs the link *and* the password (share it separately, for example by voice), so a leaked link alone doesn't open the room. The link only carries a salt (`pw=`), never the password. A wrong password shows no error: you just find nobody. The room shows 🔒 *Password*.

**Privacy**: tick *Hide members' IP addresses from each other* when creating a room and every member connects through a TURN server, so nobody in the room learns anyone's IP address (the room shows 🛡️ *IPs hidden*). It needs a TURN server from the host or the link. [`docs/PRIVACY.md`](./docs/PRIVACY.md) explains what members, relays and servers can see, and lists possible improvements.

**Versions**: members connect only with members running the same dchat protocol version. If someone in the room has a newer version, you see *Someone in this room is using a newer version of dchat* with a **Reload** button; reloading loads the latest version and keeps the room link (like any reload, it clears this tab's chat). Members on the newer version see a short notice instead. Updates that only change the interface don't affect who can connect.

**History (always on)**: whoever joins sees the room's conversation as the members see it: every message with its latest edit, reactions and file cards (*Download* while the sender is still in the room), minus deleted messages, with the names of members who already left. There is nothing to switch on, and no way to switch it off: anyone who gets in with the link (and password) can read what the room still holds, so start a new room for a fresh one. Members keep the conversation in memory only (never on disk, up to 32 MB each; the oldest messages go first) and compare what they hold on every new connection, so a member whose connection dropped for a while also gets what it missed. *Loading earlier messages…* shows while that runs; chat keeps working meanwhile. Messages travel as their authors' signed originals, bound to this room, so no member can alter them or replay messages from another room. A kick or **🔄 New Link** carries the conversation into the new room (the kicked member's earlier messages stay). It disappears when the last member leaves.

### Chat Extras

- **Typing**: *"Bo is typing…"* appears above the message box.
- **Reactions**: hover a message and tap 😀 to add 👍 ❤️ 😂 😮 😢 🎉; tap a reaction chip to add or remove yours.
- **Edit / delete your own messages**: ✏️ puts the text back in the box (Enter saves, Esc cancels) and others see *(edited)*. 🗑️ removes it for everyone. Both are signed by you; deletion is best effort, since anyone may have already read or copied the message. Members who join later see the latest edit, and deleted messages are gone for them too.
- **Private messages**: ✉️ next to a member opens a private chat. Messages are sealed with a key only the two of you can derive (ECDH between your session keys). With a direct link they travel only over that link; otherwise other members relay them without being able to read them or see whom they are for. The conversation ends when either of you leaves, because session keys are per tab.
- **@mentions**: write `@Name` and that member sees the message highlighted, hears a short chime and, if the tab is in the background, gets a `(n)` badge in the tab title (and on the app icon when dchat is installed).

### Voice Lounge

Each room has one drop-in voice lounge. Nobody is rung:
- Tap **🔊 Join Voice** to enter. Members outside voice see a short *"Ana joined voice"* prompt with a **Join** button.
- Inside, the controls are 🎙️ mic mute, 🔊 speaker mute (local only), 📹 camera (🔄 flips front/rear, or moves to the next camera once you picked one), 🖥️ screen share, ⚙️ settings and **📴 Leave**.
- **Devices**: ⚙️ settings → *Devices* picks the microphone, speaker and camera (default: the system's), in or out of voice; a change applies at once. Choices stay in memory only. Device names show once the browser may use the microphone or camera; Safari can't choose speakers, and Firefox offers its own *Choose speaker…* dialog.
- Camera and screen share are one video source at a time; switching between them reuses the same connection.
- Members with video appear in a grid (⛶ for fullscreen). Whoever is talking gets a green ring, measured locally from the audio level.
- Audio and video only flow between members who are in the lounge, directly peer-to-peer (DTLS-SRTP). A member you only reach `via` someone else is shown with ⚠: you can't hear or see each other without a direct link (see NAT below).
- **Video quality**: ⚙️ settings, or the **▾** next to 📹 and 🖥️, picks one preset for the camera and one for screen sharing. Everyone watching gets the same quality; you can change it while sending.

  | Screen share | | Camera | |
  |---|---|---|---|
  | **Fastest** | 720p, 60 fps: lowest latency, softens under load | **Smooth 60** | 720p, 60 fps (30 if the camera can't) |
  | **Smooth** | 1080p, 60 fps: keeps 60 fps, softens under load | **Balanced** (default) | 480p, 30 fps |
  | **Balanced** (default) | 1080p, 30 fps: crisp, lighter than Sharp | **HD** | 720p, 30 fps |
  | **Sharp** | full resolution, 30 fps: crisp text, fps may drop | **Full HD** | 1080p, 30 fps |
  | **Text** | full resolution, 15 fps: lowest bandwidth | **Data saver** | 360p, 15 fps |

  For games and remote control pick **Fastest** or **Smooth**; for code and documents **Sharp** or **Text**. Each viewer gets their own copy (every link has its own encoder), so higher presets cost upload and CPU per viewer; dchat uses a hardware encoder when the browser reports one. Settings live in memory only and reset on reload.
- **ⓘ on a video tile** shows live connection stats: resolution, frame rate, bitrate, round trip, buffering, decoding and an estimated latency (a lower bound: capture and display time aren't measurable). On your own tile it lists what each viewer gets and what limits it (CPU or bandwidth). Nothing is stored and no addresses are shown.
- Video is not held back to match the voice's buffer (that would delay screen shares); camera tiles are re-aligned with the voice on the viewer's side.
- **Voice limit** (default 8) and **video limit** (default 6) are set when creating the room. When the lounge is full, **Join Voice** is disabled; if two people race for the last seat, the one who joined last is moved out, using the same rule as the member limit. The limits apply to admins too.

### Sharing Files

Tap **📎**, pick a file, optionally add a caption and send. Everyone in the room sees the card:
- Each member who taps **⬇️ Download** pulls the file **directly from the sender** over their own WebRTC link. Every 64 KB chunk is sealed with ChaCha20-Poly1305 using the room key. Files are never relayed through other members or any server.
- The sender uploads to at most **2 members at a time**; others see *⏳ Queued (#n)* until a slot frees up. The sender's card shows how many are sending, waiting and done.
- **Parallel connections (experimental).** Over the internet, one WebRTC connection usually fills only a few MB/s of a much faster line: it slows down sharply with round-trip time and packet loss. Large uploads therefore start on the normal link and add extra connections to the downloader, one at a time, while each one still raises the speed, up to the cap in **⚙️ Settings → Files** (1 turns this off; the default allows 8). The cap works from either computer and applies at once, even mid-transfer. Both cards show the live speed and how many connections carry it, and the browser console logs each step (`Upload to …: AddLink at 5.1 MB/s over 1 connections`). The extra connections carry file chunks only, use the room's STUN/TURN settings, and close 15 s after the last upload to that member.
- The sender can **Withdraw** the offer for everyone, which also stops transfers in progress.
- Downloads stream to disk when the browser supports the File System Access API; otherwise they are assembled in memory (with a warning above 250 MB).
- A finished download shows its size, how long it took (from the first byte) and its average speed, and offers **⬇️ Download again** while the sender still offers the file. During a transfer, the speed shown is averaged over the last 5 s.
- On iPhone and iPad a finished download waits on the card: tap **💾 Save** to open the share sheet (Save to Files, AirDrop, …) over dchat. Closing the sheet keeps the button. dchat never hands the file over unasked, because iOS would open it in another app and suspend dchat, which drops you from the room.
- If you have no direct link to the sender (`via` in the member list), the card says *Sender not directly reachable*. If the sender leaves, pending offers are marked unavailable and running transfers stop.

### Remote Control

While you share your **entire screen** in the lounge, you can let someone else use your mouse and keyboard, and let several people play with game controllers. Browsers can't move the mouse, press keys or plug in controllers on a computer, so the shared computer runs a small companion app, **`dchat-host`**, on Linux or Windows.

1. **Start dchat-host** on the computer you share. The **🖱️** dialog links to the download (Linux and Windows; check it against `SHA256SUMS`), or build it with `cargo build -p host-agent --release`. On Windows, double-click `dchat-host.exe`; on Linux, run `./install.sh` once, then `./dchat-host`. If it asks which dchat site may connect, type your site's address (builds made for your site don't ask; `--allow-origin` also works). It prints a one-time code such as `K7QM-4XPA`.
   - Linux: `install.sh` (in the download, or `crates/host-agent/dist/`) lets the logged-in user create the virtual devices through `/dev/uinput`, asking for your password once; `./install.sh --uninstall` undoes it.
   - Windows: nothing to install for mouse and keyboard. For controllers, install the [ViGEmBus driver](https://github.com/nefarius/ViGEmBus/releases) once. Windows of apps running as administrator (and UAC prompts or the lock screen) can't be controlled unless dchat-host also runs as administrator.
2. **Connect it**: in voice, tap **🖱️**, type the code and **Connect**. Chrome may ask to allow access to apps on this device: allow it.
3. **Others ask, you decide**: members watching your screen see **🖱️ Request control** on your tile. You get a prompt with **Allow** / **Deny**. One person at a time has mouse and keyboard; allowing someone new takes it from the previous one.
4. **Controlling**: click the shared screen to start. Your pointer, clicks, wheel and keys go to the shared computer (keys by position, so the shared computer's keyboard layout applies). **Ctrl+Alt+Shift+Q**, leaving the tab or **Stop controlling** gives control back.
   - **Desktop mode** (default): you click where you point.
   - **Game mode** (the 🎮 button on the tile): your mouse is captured and moves the view, as games expect. The video quality stays whatever the sharer picked: for games, the sharer should choose **Fastest** or **Smooth**. Press Esc (or Ctrl+Alt+Shift+Q) to get your mouse back. In fullscreen, Chrome also passes keys like Esc and Alt+Tab to the shared computer; hold Esc to leave.
5. **Controllers**: members can also tap **🎮 Request controller**. Each person you allow gets their own virtual Xbox 360 controller on your computer, P1 to P4, so up to four can play at once (one of them may also have mouse and keyboard). Their browser sends whatever "standard" controller they have connected; if nothing happens, they press a button on it once (browsers only show a controller after that). No rumble yet.
6. **Stopping**: **Ctrl+Alt+Shift+Q** anywhere on the shared computer (Windows and X11 desktops), **⛔ Stop control** in the lounge bar, the **✕** next to a member, or Enter / Ctrl+C in dchat-host's terminal. Control also ends when you stop sharing, leave voice, or the app disconnects, and everything held down is released.

Security: dchat-host only listens on `127.0.0.1`, only accepts your dchat site, and pairs only with someone who types its code (both sides prove they know it). Your tab forwards input only from the person you allowed, and the app checks that again. Mouse and keyboard give full use of your computer, including allowing others: only allow people you trust.

### Installing dchat as an App

dchat can be installed like an app (a PWA): it gets its own window and icon, and starts from the icon. It is the same site: nothing is stored, a reload still wipes the session, and installing changes nothing about who sees what.

- **Desktop (Chrome, Edge)**: the start page shows **📲 Install app** (or use the install icon in the address bar). Room links you click elsewhere then open in a new dchat window; the browser's app settings can turn that off.
- **Android (Chrome)**: **📲 Install app**, or the menu's *Install app*. Room links open in the app; since it has one window, tapping a link while you are in a room asks before leaving it.
- **iPhone, iPad**: Share → **Add to Home Screen**; on a Mac, Safari's File → **Add to Dock**. Add it from the start page rather than from inside a room, so the icon never holds a room link. iOS opens room links in Safari, never in the app: copy the link instead and paste it into **Join with a link**.
- **Join with a link** (on the start page, everywhere): paste a room link from any dchat site, or just its `#room=…&key=…` part. Only the part after `#` is used, and the room is joined from this site.

Installing needs HTTPS (or `localhost`): browsers don't offer it on the dev server's self-signed LAN address. Remote control still needs **`dchat-host`** on the shared computer, installed app or not: no web app can move the mouse or press keys.

### URL Fragment Parameters

Everything after `#` stays in the browser and is never sent to any server.

| Parameter | Example | Meaning |
|---|---|---|
| `room` | `room=jr9m4r26` | Room ID (hashed before it reaches relays) |
| `key` | `key=Zm9v…` | 256-bit room key (base64url) |
| `adm` | `adm=9f3c…` | Admin public key; sessions proving it get the `ADMIN` badge and a guaranteed seat |
| `admsk` | `admsk=…` | Admin secret key: **admin links only** (the creator's, or a member who took over or was made admin), never in the invite or QR code |
| `max` | `max=10` | Member limit; default 25, `0` = unlimited (no hard ceiling; large rooms load every member) |
| `maxa` | `maxa=4` | Voice limit: members in the lounge at once; default 8, `0` = unlimited |
| `maxv` | `maxv=2` | Video limit: cameras/screens on at once; default 6, `0` = unlimited |
| `hist` | `hist=1` | Ignored: history is always on. Links from older versions that carry it still work |
| `relays` | `relays=wss://a,nostr` | The room's Nostr relays, exactly; `nostr` stands for the public relays (default when absent) |
| `turn` | `turn=turns:turn.example.com:5349` | Optional TURN server(s), comma-separated |
| `turnuser`, `turnpass` | `turnuser=me&turnpass=s3cret` | TURN credentials (percent-encode special characters) |
| `stun` | `stun=stun:stun.example.com:3478` | The room's STUN server(s), comma-separated. Without it, the host's STUN is used, and only if there is none, Google's |
| `hideip` | `hideip=1` | Connect only through TURN, so members never see each other's IP addresses (needs a TURN server) |
| `pw` | `pw=q8Zt…` | Password room: random salt; the room key also needs the password, which is never in the link |

### When Members Can't Connect Directly (NAT)

Without TURN, dchat uses STUN only, so it needs no infrastructure of its own. Most home and office networks connect fine. But two members behind **carrier-grade NAT** (common on mobile data) or **symmetric NAT** often cannot open a direct WebRTC link: roughly 10–20% of pairs. In a group mesh, the more members a room has, the more likely it is that some pair fails.

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

### 2. Nostr Relays: Public or Your Own
By default, rooms use a pool of public Nostr relays:
- `wss://relay.damus.io`
- `wss://nos.lol`
- `wss://relay.primal.net`

When creating a room, **Signaling relays** lets you choose: the public relays (default), **My relay**, or **My relay + public Nostr relays (backup)**. The choice goes into the room link, so every member uses the same relays:
```
https://your-domain.com/#room=…&key=…&relays=wss://relay.example.com            # only your relay
https://your-domain.com/#room=…&key=…&relays=wss://relay.example.com,nostr      # yours + public (nostr = the public relays)
```
Click the **⚡ Nostr** badge to see the room's relays and how many are connected. Dropped relay connections reconnect automatically.

Relays are used only to find members and set up each direct link. Once two members are linked, everything else, including starting voice, camera or screen share, travels over their own link. If the relays go down, members already connected keep chatting and calling; only newcomers (and links that need to reconnect) wait for a relay.

To run your own relay, this repository includes **`dchat-relay`** (`crates/relay`): a small RAM-only relay that forwards only dchat's ephemeral signaling events, with signature, freshness and rate checks and an optional origin lock. It ships as a Docker image with a Caddy setup for automatic `wss://`. See [`docs/DEPLOYMENT.md`](./docs/DEPLOYMENT.md) section 4.

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
- Tap **"💥 Wipe Session"** or refresh the browser: this phone's copy of the conversation is destroyed from RAM. Entering again brings back what the other phone still holds; once both have left, nothing of it remains anywhere.

---

## Automated Verification & Testing

The repository contains a multi-tiered test suite for automated CI/CD and AI-driven development:

### 1. Protocol & Crypto Unit Tests
Verifies 256-bit key generation, ChaCha20-Poly1305 encryption/decryption roundtrips, bad nonce/tamper rejection, and room state machine:
```bash
cargo test --workspace
```

### 2. Playwright Multi-Browser End-to-End (E2E) Tests
Simulates several isolated browser members: WebRTC mesh handshakes, signed message fan-out, member caps and the admin seat, text relayed between members without a direct link, history synced to whoever joins or reconnects, admin succession and Make admin, empty `localStorage`/`sessionStorage`, and memory wipe on reload.

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
