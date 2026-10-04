# Privacy in dchat

Messages, voice, video and files are end-to-end encrypted, and nothing is stored: a reload wipes everything. This page covers the rest, the metadata: who can see your IP address, when you're online, and who you talk to.

## Who sees what

| Who | Can see | Never sees |
|---|---|---|
| **Other members** (anyone with the link) | Your name, your messages, and your IP address from the direct WebRTC link. Once you allow the microphone or camera, browsers also reveal your local network address. In a room that hides IP addresses (`hideip=1`) they see only the TURN server's address. | Your private messages to others |
| **Nostr relays** | Your IP address, the room's topic (a hash of the room ID), your session's public key, and when you join, stay (a presence beacon every 15 s) and leave | The room ID, the key, names, messages, and your handshakes (sealed, see below) |
| **The STUN server** | Your IP address each time you open a connection | Anything about the room |
| **A TURN server** (when used) | The IP addresses of members whose traffic it relays, and how much and when | Content: it only forwards encrypted packets |
| **The website host** | Your IP address when you load the page (and, on Cloudflare, when the app asks for TURN credentials) | The part of the link after `#`, which browsers never send |
| **Someone who gets the link later** | Can join the room while it exists. From recorded relay traffic, only presence beacons: handshakes were sealed to session keys that died with the tabs. | Messages sent before they joined (unless the room shares history) |

## Protections

- **End-to-end encryption.** WebRTC encrypts every link (DTLS, SRTP). On top of that, room messages are encrypted with the room key and signed by their author, and private messages are sealed between the two members' session keys.
- **Sealed handshakes.** The offers, answers and ICE candidates that set up a link contain IP addresses. Through the relays they are encrypted with the room key **and** sealed to the recipient's session key (`RelaySignal::Sealed`). Other members can't read them, and neither can anyone who later gets the link and kept relay traffic. Session keys exist only in the tab's memory. Once two members are linked, later handshakes travel over their own link.
- **Google's STUN server only as a last resort.** A member uses the room's STUN server (`&stun=`), otherwise the host's (the Cloudflare setup provides one), and only otherwise `stun.l.google.com`.
- **Hide IP addresses (`hideip=1`).** Tick *Hide members' IP addresses from each other* when creating a room. Every member then connects only through a TURN server, so other members see the TURN server's address instead of yours, and your local network address is never gathered.
  - It needs a TURN server: deploy on Cloudflare (the site provides one), or add `&turn=` to the link. Without one, the room shows a banner and nobody can connect.
  - All traffic then goes through TURN, which costs bandwidth and adds some delay.
  - It hides you from members, not from relays or the TURN server, which still see your IP address. Use a VPN for that.
- **No long-term identity.** Each tab makes a fresh session key; nothing ties two visits together.
- **Short-lived relay events.** Signaling uses ephemeral Nostr events (kind 20001), which compliant relays forward without storing. `dchat-relay` stores nothing and logs no IP addresses.

## Possible improvements

Not implemented yet, roughly in order of value for effort.

1. **Passphrase rooms.** Derive the room key from the link *and* a passphrase shared separately, so a link leaked through a chat app, browser history or sync is useless on its own. It needs a passphrase prompt and a slow key derivation, such as PBKDF2 or Argon2.
2. **Keep the key out of the address bar.** After joining, remove `key` and `admsk` from the address bar (`history.replaceState`) and keep them only in memory; Copy Link and the QR code still work. This keeps the key out of later history entries, browser sync, tab restore and screenshots, although some browsers may already have recorded the first visit. The trade-off is that a reload returns to the lobby.
3. **Show relays less.**
   - Pad signals to a few fixed sizes, so relays can't tell a presence beacon from a handshake.
   - Sign each relay event with a throwaway key and carry the session key inside the encryption, so relays can't count members or follow a session.
   - Send fewer presence beacons once connected.
   - A bigger change: only one or two members stay on the relays to introduce newcomers, and newcomers' other handshakes travel over the mesh. Relays would then see two IP addresses per room instead of everyone's.
4. **Host-chosen default relays.** Let a self-hosted site default to its own relay (for example from a small config file next to the app), so its rooms don't use public relays unless asked to.
5. **Hide only my IP address.** `hideip` is set for the whole room by its creator, but the TURN-only setting works per member. A join-screen switch could let one member hide their own address in any room.
6. **A STUN server in the relay deployment.** A STUN-only service (for example coturn) in `deploy/relay`, so self-hosters on static hosts can set `&stun=` to their own server.
7. **Don't send "is typing".** A per-member switch.
8. **Content-Security-Policy.** No external code is loaded today; a strict policy would also limit where injected code could send data. Custom relays (`wss://` anywhere) limit how strict it can be.

## Limits that can't be fixed in the app

- Anyone with the link is in the room, sees everyone who is there, and can screenshot or copy what they see.
- Deleted and edited messages are only removed by unmodified clients.
- Relays, STUN and TURN servers and the website host see IP addresses. Use a VPN to hide yours from them.
- Tor Browser disables WebRTC, so dchat can't run there.
