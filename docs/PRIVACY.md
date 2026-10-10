# Privacy in dchat

Messages, voice, video and files are end-to-end encrypted, and nothing is written to disk: a reload wipes your tab's copy, and the room's conversation lives only in the memory of the members still in it. This page covers the rest, the metadata: who can see your IP address, when you're online, and who you talk to.

## Who sees what

| Who | Can see | Never sees |
|---|---|---|
| **Other members** (anyone who gets in) | Your name, your messages (including the ones you sent before they joined, see [Room history](#room-history-always-on)), and your IP address from the direct WebRTC link. Once you allow the microphone or camera, browsers also reveal your local network address. In a room that hides IP addresses (`hideip=1`) they see only the TURN server's address. When a private message has to be relayed, the members relaying it see that you sent one and roughly how long it is. They also see when an admin hands the admin secret to someone (for succession or **Make admin**), but not to whom. They see when you drop out of reach without leaving (`away`, for example while your phone is in another app) and when you are back. | The text of private messages between others, and whom they were sent to |
| **Nostr relays** | Your IP address, the room's topic (a hash), a relay key that is new every session (your member identity travels inside the encryption), and when you join, stay (a presence beacon every 15 s), come back after a pause and leave. After an admin moves the room, members who move keep listening on the old topic for up to 5 minutes if someone was away, so timing can hint that the two topics belong together (as members' IP addresses already do). | The room ID, the key, names, messages, your handshakes and a rekey handed to a member who was away (both sealed, see below) |
| **The STUN server** | Your IP address each time you open a connection | Anything about the room |
| **A TURN server** (when used) | The IP addresses of members whose traffic it relays, and how much and when | Content: it only forwards encrypted packets |
| **The website host** | Your IP address when you load the page (and, on Cloudflare, when the app asks for TURN credentials) | The part of the link after `#`, which browsers never send |
| **dchat-host** (only if you run it to allow remote control) | Input from the members you allow, their names, and your shared screen's size. It listens only on `127.0.0.1` and talks only to your own dchat tab. | Anything else in the room. It connects nowhere, writes no files and logs no input. |
| **Someone who gets the link later** | Can join the room while it exists, unless it has a password, and then reads the conversation the members still hold. From recorded relay traffic, only presence beacons: handshakes were sealed to session keys that died with the tabs. | In a password room without the password: anything at all, not even the room's relay traffic |

## Protections

- **End-to-end encryption.** WebRTC encrypts every link (DTLS, SRTP). On top of that, room messages are encrypted with the room key and signed by their author.
- **Room passwords (optional).** Set a password when creating a room, and share it separately from the link, for example by voice. It is opt-in: the password box appears only after ticking **Protect with a password**, and it asks for a new password (`autocomplete="new-password"`), so a password the browser saved earlier never turns a new room into a password room.
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
- **Extra file connections reveal nothing new.** A large upload may open up to 7 more connections to the member downloading it (**⚙️ Settings → Files**, 1 turns this off). They are set up over the two members' existing link, never through the relays, follow the room's STUN/TURN settings (only TURN with `hideip=1`), and carry nothing but sealed file chunks. Each one does make its own STUN requests and, through TURN, takes its own allocation, so a STUN or TURN server can see that a large transfer is under way.
- **Photos, videos and voice messages.** What you share is pulled from your device by each member who views it, like any file. Pictures, voice messages and other media up to 16 MB load by themselves once their card is on a member's screen, so your card's counts show who has looked at them. The card itself carries a small blurred thumbnail drawn from the picture (none of the original's metadata), its pixel size and length, and for a voice message its waveform: like the file's name, these are in the room's history. Loaded media stays in the viewer's memory only, never on disk, until they save it. Voice recordings stay in your tab's memory, offered from there; nothing is sent before you tap ➤.
- **Photo location warning.** A JPEG that records where it was taken (EXIF GPS, or an XMP position) is flagged on its chip before sending; **Remove location** zeroes the GPS data in place and drops XMP positions, leaving the picture and its orientation untouched. Other metadata (camera model, time) and other formats (HEIC, PNG, WebP) still go out as they are.
- **Nothing typed is kept by the browser.** Forms and text boxes turn off autofill (`autocomplete="off"`; the create screen's password box uses `new-password` instead, which browsers never fill), so names, passwords and relay addresses aren't saved to the browser's form history.
- **Spell checking can be turned off.** Some browsers' enhanced spell check (Chrome's, Edge's) sends what you type to Google or Microsoft. **⚙️ Settings → Spell check while typing** turns spell checking off for the message boxes. It is on by default.
- **Remote control only with your click.** Members can ask to control your mouse and keyboard (one person at a time) or to plug in a game controller while you share your screen, but nothing happens until you allow it. Their input reaches only your computer, sealed over your direct link; your tab and `dchat-host` both drop anything from someone you didn't allow. Everything held down is released when control ends.
- **Video quality and stats stay on your device.** The camera and screen presets are local settings: nothing about them is sent to anyone beyond what the video itself shows. Choosing a codec asks your own browser which encoders it has (`MediaCapabilities`); members can infer from the negotiated codec whether you have, say, a hardware H.265 encoder, but WebRTC's handshake already listed your browser's codecs to them. The ⓘ stats panel reads only your own connection's statistics, never shows addresses or candidates, and stores nothing.
- **Installing the app changes nothing.** The installed app (PWA) is the same site under the same rules: nothing is stored, and the cache holds only the app's own files and icons. A room link opened in the app, or pasted into **Join with a link**, is read on your device; only its part after `#` is used, written into the address in place and never sent. The app keeps no list of rooms. The badge on its icon is a count of unread @mentions while it is open.
- **The admin secret moves sealed.** For succession, an admin keeps a copy of the room's admin secret with the member who has been there longest, sealed to that member's session key, like a private message and without naming them. That member holds it in memory only and uses it only once no admin has been in the room, or away, for 15 seconds. Until then nobody, that member included, is shown who holds it. By design the secret is then on two devices, and **Make admin** adds one more.
- **No long-term identity.** Each tab makes a fresh session key; nothing ties two visits together. When an admin moves the room (kick or new link), you keep your key inside the room, so you stay the author of your earlier messages, but your relay events are signed by a new key, so relays can't tie the new room to the old one.
- **Away, not stored.** When your phone pauses the tab (another app, the screen locked), the others keep your place for 5 minutes. Nothing is written anywhere for that: your key stays in the paused tab's memory, a DM sent to you waits sealed in the sender's tab, and a rekey made meanwhile is handed to you sealed to your session key (`RekeyForward`), never readable with the room key. If the phone closes the tab instead, you come back as a new member.
- **Short-lived relay events.** Signaling uses ephemeral Nostr events (kind 20001), which compliant relays forward without storing. `dchat-relay` stores nothing and logs no IP addresses.

## Room history (always on)

Whoever joins a room sees its conversation as the members see it. There is no switch: start a new room for a fresh one.

- **What is shared:** messages with their latest edit, reactions, file cards (name, size, type and caption, and for pictures, videos and voice messages a small blurred thumbnail, their size and length or waveform; the file itself only from its sender, while they are in the room) and the names of members who already left. Never private messages, typing, voice or anything else.
- **Who gets it:** anyone who gets in with the link (and the password, in a password room), while at least one member is still there. A member whose connection dropped for a while also gets what it missed.
- **Where it lives:** in each member's memory only, never on disk, up to 32 MB per member (the oldest messages go first). It survives a kick or a new link, and the kicked member's earlier messages stay in it. It is gone once the last member leaves.
- **What protects it:** every message travels as its author's signed original, bound to this room, so no member can alter it, put words in someone else's mouth, or replay messages from another room. A deleted message is dropped for good and can't be brought back by a member who still holds it.
- **Limits:** deletions and edits are honored only by unmodified clients, and anyone present can copy what they see.

## Possible improvements

Not implemented yet. Roughly in order of value for effort within each group.

### Links and keys
- **Keep the key out of the address bar.** After joining, remove `key` and `admsk` from the address bar (`history.replaceState`) and keep them only in memory; Copy Link and the QR code still work. This keeps the key out of later history entries, browser sync, tab restore and screenshots, although some browsers may already have recorded the first visit. The trade-off is that a reload returns to the lobby.
- **Rotating relay topics.** Derive the topic from the room key and the current time period, so relays can't follow a long-lived room across days and the room ID alone doesn't lead to it.
- **Verify members out of band.** Compare a few safety words derived from both session keys to confirm a member is who you think, rather than someone with the link and the same display name, before sending private messages.
- **Rotate the admin key on a kick.** Someone who once held the admin secret (a former heir, or a kicked one) keeps knowing it, and the key stays the same across new links. A kick could move the room to a fresh admin key, handed sealed to the remaining admins.

### What relays and the network see
- **Show relays less.**
  - Pad signals to a few fixed sizes, so relays can't tell a presence beacon from a handshake.
  - Relay events are signed with a key that is new every session, and the member identity travels inside the encryption. A new key for every event would also stop relays from counting members or following a session.
  - Send fewer presence beacons once connected.
  - A bigger change: only one or two members stay on the relays to introduce newcomers, and newcomers' other handshakes travel over the mesh. Relays would then see two IP addresses per room instead of everyone's.
- **Host-chosen default relays.** Let a self-hosted site default to its own relay (for example from a small config file next to the app), so its rooms don't use public relays unless asked to.
- **A STUN server in the relay deployment.** A STUN-only service (for example coturn) in `deploy/relay`, so self-hosters on static hosts can set `&stun=` to their own server.
- **Constant-bitrate audio.** With Opus's variable bitrate and silence suppression, packet sizes follow your speech, and TURN servers or network observers can partly infer what is said. Constant bitrate closes that leak at the cost of bandwidth.
- **Privacy-first defaults set by the host.** For example, a Cloudflare deployment with TURN could make "hide IP addresses" the default for new rooms.

### What other members see
- **Keep local network addresses private** even without `hideip`. Browsers stop masking the local IP once mic or camera permission is granted, so joining voice reveals it to everyone. dchat could drop private-address candidates before sending them; two members on the same Wi-Fi might then need router hairpinning or TURN.
- **Hide only my IP address.** `hideip` is set for the whole room by its creator, but the TURN-only setting works per member. A join-screen switch could let one member hide their own address in any room.
- **Strip all photo metadata.** dchat flags and removes a JPEG's location (see *Photo location warning*), but other EXIF fields and other formats (HEIC, PNG, WebP) go out as they are. Removing all of it before sending, with a switch to keep it, would close that.
- **Pad private messages.** Relaying members see a private message's size; padding to fixed sizes would hide how long it is.
- **Join voice muted.** Joining voice turns the microphone on right away. Starting muted, or a "join muted" setting, prevents accidentally broadcasting a room.
- **Safer screen sharing.** Ask the browser to leave the dchat tab out of the share picker and to suggest a single window rather than the whole screen, which also shows notifications.
- **Don't send "is typing".** A per-member switch.
- **Disappearing messages.** A per-room timer (`&ttl=`) that removes messages from every screen, and from the room's history, after a while, enforced by honest clients. It protects long-open tabs from shoulder-surfing and later screenshots, and limits what someone who gets the link later can read.
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
- Room history can't be turned off: whoever gets in reads what the room still holds.
- Admin rights can't be taken back: a member who was made admin, or held the admin secret as heir, keeps knowing it.
- Relays, STUN and TURN servers and the website host see IP addresses. Use a VPN to hide yours from them.
- Browsers may still offer to save a room password in their password manager; decline if you don't want it kept.
- Tor Browser disables WebRTC, so dchat can't run there.
