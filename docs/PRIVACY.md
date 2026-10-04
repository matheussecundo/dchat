# Privacy in dchat

Messages, voice, video and files are end-to-end encrypted, and nothing is stored: a reload wipes everything. This page covers the rest, the metadata: who can see your IP address, when you're online, and who you talk to.

## Who sees what

| Who | Can see | Never sees |
|---|---|---|
| **Other members** (anyone who gets in) | Your name, your messages, and your IP address from the direct WebRTC link. Once you allow the microphone or camera, browsers also reveal your local network address. In a room that hides IP addresses (`hideip=1`) they see only the TURN server's address. When a private message has to be relayed, the members relaying it see that you sent one and roughly how long it is. | The text of private messages between others, and whom they were sent to |
| **Nostr relays** | Your IP address, the room's topic (a hash), your session's public key, and when you join, stay (a presence beacon every 15 s) and leave | The room ID, the key, names, messages, and your handshakes (sealed, see below) |
| **The STUN server** | Your IP address each time you open a connection | Anything about the room |
| **A TURN server** (when used) | The IP addresses of members whose traffic it relays, and how much and when | Content: it only forwards encrypted packets |
| **The website host** | Your IP address when you load the page (and, on Cloudflare, when the app asks for TURN credentials) | The part of the link after `#`, which browsers never send |
| **dchat-host** (only if you run it to allow remote control) | Input from the members you allow, their names, and your shared screen's size. It listens only on `127.0.0.1` and talks only to your own dchat tab. | Anything else in the room. It connects nowhere, writes no files and logs no input. |
| **Someone who gets the link later** | Can join the room while it exists, unless it has a password. From recorded relay traffic, only presence beacons: handshakes were sealed to session keys that died with the tabs. | In a password room without the password: anything at all, not even the room's relay traffic |

## Protections

- **End-to-end encryption.** WebRTC encrypts every link (DTLS, SRTP). On top of that, room messages are encrypted with the room key and signed by their author.
- **Room passwords (optional).** Set a password when creating a room, and share it separately from the link, for example by voice.
  - The room key is derived from the key in the link *and* the password (Argon2id, then SHA-256), so a link that leaks through a chat app, browser history or sync is useless on its own.
  - The link holds only a random salt (`pw=`). The relay topic also depends on the password, so someone with just the link can't even find the room's relay traffic.
  - The password is typed on the join screen, stretched once and cleared from the field; only the stretched value stays in memory, which lets members follow a new link (kick or rotate) without retyping it.
  - A wrong password shows no error: whoever typed it simply finds nobody.
  - The password protects only as well as it is chosen. Someone with the link and recorded relay traffic can try guesses offline, and Argon2id makes each guess slow, not impossible.
- **Sealed handshakes.** The offers, answers and ICE candidates that set up a link contain IP addresses. Through the relays they are encrypted with the room key **and** sealed to the recipient's session key (`RelaySignal::Sealed`). Other members can't read them, and neither can anyone who later gets the link and kept relay traffic. Session keys exist only in the tab's memory. Once two members are linked, later handshakes travel over their own link.
- **Private messages that don't name their recipient.** A private message is sealed with a key only its two members can derive. With a direct link it goes only to the recipient; otherwise it is relayed through the room without a recipient field. Every member tries to open it, only the recipient can, and they don't pass it on.
- **Google's STUN server only as a last resort.** A member uses the room's STUN server (`&stun=`), otherwise the host's (the Cloudflare setup provides one), and only otherwise `stun.l.google.com`.
- **Hide IP addresses (`hideip=1`).** Tick *Hide members' IP addresses from each other* when creating a room. Every member then connects only through a TURN server, so other members see the TURN server's address instead of yours, and your local network address is never gathered.
  - It needs a TURN server: deploy on Cloudflare (the site provides one), or add `&turn=` to the link. Without one, the room shows a banner and nobody can connect.
  - All traffic then goes through TURN, which costs bandwidth and adds some delay.
  - It hides you from members, not from relays or the TURN server, which still see your IP address. Use a VPN for that.
- **Nothing typed is kept by the browser.** Forms and text boxes turn off autofill (`autocomplete="off"`), so names, passwords and relay addresses aren't saved to the browser's form history.
- **Spell checking can be turned off.** Some browsers' enhanced spell check (Chrome's, Edge's) sends what you type to Google or Microsoft. **⚙️ Settings → Spell check while typing** turns spell checking off for the message boxes. It is on by default.
- **Remote control only with your click.** Members can ask to control your mouse and keyboard while you share your screen, but nothing happens until you allow it, and only one person at a time. Their input reaches only your computer, sealed over your direct link; your tab and `dchat-host` both drop anything from someone you didn't allow. Everything held down is released when control ends.
- **No long-term identity.** Each tab makes a fresh session key; nothing ties two visits together.
- **Short-lived relay events.** Signaling uses ephemeral Nostr events (kind 20001), which compliant relays forward without storing. `dchat-relay` stores nothing and logs no IP addresses.

## Possible improvements

Not implemented yet. Roughly in order of value for effort within each group.

### Links and keys
- **Keep the key out of the address bar.** After joining, remove `key` and `admsk` from the address bar (`history.replaceState`) and keep them only in memory; Copy Link and the QR code still work. This keeps the key out of later history entries, browser sync, tab restore and screenshots, although some browsers may already have recorded the first visit. The trade-off is that a reload returns to the lobby.
- **Rotating relay topics.** Derive the topic from the room key and the current time period, so relays can't follow a long-lived room across days and the room ID alone doesn't lead to it.
- **Verify members out of band.** Compare a few safety words derived from both session keys to confirm a member is who you think, rather than someone with the link and the same display name, before sending private messages.

### What relays and the network see
- **Show relays less.**
  - Pad signals to a few fixed sizes, so relays can't tell a presence beacon from a handshake.
  - Sign each relay event with a throwaway key and carry the session key inside the encryption, so relays can't count members or follow a session.
  - Send fewer presence beacons once connected.
  - A bigger change: only one or two members stay on the relays to introduce newcomers, and newcomers' other handshakes travel over the mesh. Relays would then see two IP addresses per room instead of everyone's.
- **Host-chosen default relays.** Let a self-hosted site default to its own relay (for example from a small config file next to the app), so its rooms don't use public relays unless asked to.
- **A STUN server in the relay deployment.** A STUN-only service (for example coturn) in `deploy/relay`, so self-hosters on static hosts can set `&stun=` to their own server.
- **Constant-bitrate audio.** With Opus's variable bitrate and silence suppression, packet sizes follow your speech, and TURN servers or network observers can partly infer what is said. Constant bitrate closes that leak at the cost of bandwidth.
- **Privacy-first defaults set by the host.** For example, a Cloudflare deployment with TURN could make "hide IP addresses" the default for new rooms.

### What other members see
- **Keep local network addresses private** even without `hideip`. Browsers stop masking the local IP once mic or camera permission is granted, so joining voice reveals it to everyone. dchat could drop private-address candidates before sending them; two members on the same Wi-Fi might then need router hairpinning or TURN.
- **Hide only my IP address.** `hideip` is set for the whole room by its creator, but the TURN-only setting works per member. A join-screen switch could let one member hide their own address in any room.
- **Strip photo metadata.** Phone photos often carry GPS coordinates in their EXIF data, and shared files go out as they are. Removing JPEG and PNG metadata before sending would stop that, with a switch to keep it.
- **Pad private messages.** Relaying members see a private message's size; padding to fixed sizes would hide how long it is.
- **Join voice muted.** Joining voice turns the microphone on right away. Starting muted, or a "join muted" setting, prevents accidentally broadcasting a room.
- **Safer screen sharing.** Ask the browser to leave the dchat tab out of the share picker and to suggest a single window rather than the whole screen, which also shows notifications.
- **Don't send "is typing".** A per-member switch.
- **Disappearing messages.** A per-room timer (`&ttl=`) that removes messages from every screen after a while, enforced by honest clients. It protects long-open tabs from shoulder-surfing and later screenshots.
- **Camera background blur.** It would need a segmentation model running in WebAssembly, so it is heavy.

### On your device
- **Panic shortcut and privacy screen.** A keyboard shortcut that wipes the session and leaves, and optionally blurring the chat when the tab loses focus.

### Trust in the code
- **Verifiable builds.** This is the biggest remaining trust assumption. All the encryption depends on the host serving honest code; a compromised host could serve a build that reads the key from the link. Options:
  - reproducible builds with published hashes;
  - hosting on IPFS, where the address is a hash of the code;
  - a small verifier.

  The integrity hashes Trunk adds don't help here, since the same host serves both the hashes and the code.
- **Content-Security-Policy.** No external code is loaded today; a strict policy would also limit where injected code could send data. Custom relays (`wss://` anywhere) limit how strict it can be.

## Limits that can't be fixed in the app

- Anyone who gets in sees everyone who is there, and can screenshot or copy what they see.
- Allowing someone your mouse and keyboard gives them full use of your computer while it lasts, including approving others. Only allow people you trust.
- Deleted and edited messages are only removed by unmodified clients.
- Relays, STUN and TURN servers and the website host see IP addresses. Use a VPN to hide yours from them.
- Browsers may still offer to save a room password in their password manager; decline if you don't want it kept.
- Tor Browser disables WebRTC, so dchat can't run there.
