//! Local network addresses stay private. Once a page may use the microphone or camera,
//! browsers stop masking host candidates behind mDNS names, so every member would learn
//! everyone's LAN address (192.168.x, 10.x, fc00::/7…). These rules keep them back: a
//! candidate with a local address (host, or reflexive from a STUN or TURN server on the LAN)
//! goes only to a member behind the same public address (the same
//! network, where it is what lets two devices link), or when we have no public address at
//! all (offline LAN, STUN blocked: today's behavior). Related addresses (`raddr`) and SDP
//! address lines never carry a private address. Local behavior only, not on the wire.
//! Pure Rust, no browser APIs.

use crate::messages::IceCandidateData;
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Private candidates are released after this long without a public address of our own
/// (`AddressGate::timeout`): STUN is slow or blocked.
pub const RELEASE_WITHOUT_PUBLIC_MS: u32 = 2000;

/// Whether `ip` is a local network address: private IPv4 ranges, carrier-grade NAT space,
/// link-local, IPv6 unique-local (and IPv4-mapped forms of those). Loopback isn't: it never
/// leaves the device.
pub fn is_local_address(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_local_v4(v4),
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80 || v6.to_ipv4_mapped().is_some_and(|v4| is_local_v4(&v4))
        }
    }
}

fn is_local_v4(v4: &Ipv4Addr) -> bool {
    let [a, b, ..] = v4.octets();
    v4.is_private() || v4.is_link_local() || (a == 100 && (64..128).contains(&b))
}

fn is_public(ip: &IpAddr) -> bool {
    !is_local_address(ip) && !ip.is_loopback() && !ip.is_unspecified() && !ip.is_multicast()
}

/// What a candidate reveals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateKind {
    /// Our own local network address: a host candidate, or a reflexive one seen by a STUN or
    /// TURN server on the same network. Held back (`AddressGate`).
    Local,
    /// Our public address: a server-reflexive candidate, or a host one with a global address.
    Public(IpAddr),
    /// Everything else (mDNS names, loopback, relay addresses, which are the TURN server's):
    /// sent as is.
    Other,
}

/// The `(address, type)` of a candidate line (`candidate:…` or `a=candidate:…`).
fn address_and_type(candidate: &str) -> Option<(&str, &str)> {
    let fields: Vec<&str> = candidate.split_whitespace().collect();
    let address = *fields.get(4)?;
    let typ = fields.iter().position(|f| *f == "typ").and_then(|i| fields.get(i + 1))?;
    Some((address, typ))
}

pub fn classify(candidate: &str) -> CandidateKind {
    let Some((address, typ)) = address_and_type(candidate) else {
        return CandidateKind::Other;
    };
    let Ok(ip) = address.parse::<IpAddr>() else {
        // mDNS names (`….local`) hide the address already.
        return CandidateKind::Other;
    };
    match typ {
        "host" | "srflx" | "prflx" if is_local_address(&ip) => CandidateKind::Local,
        "host" | "srflx" if is_public(&ip) => CandidateKind::Public(ip),
        _ => CandidateKind::Other,
    }
}

/// `candidate` without its related address: `raddr`/`rport` name the member's host (or, for
/// a relay candidate, its public) address, which nobody needs to connect.
pub fn scrub_related(candidate: &str) -> String {
    let mut fields: Vec<String> = candidate.split(' ').map(str::to_string).collect();
    let v6 = address_and_type(candidate).is_some_and(|(address, _)| address.parse::<Ipv6Addr>().is_ok());
    for i in 0..fields.len().saturating_sub(1) {
        match fields[i].as_str() {
            "raddr" => fields[i + 1] = if v6 { "::".into() } else { "0.0.0.0".into() },
            "rport" => fields[i + 1] = "0".into(),
            _ => {}
        }
    }
    fields.join(" ")
}

/// An outgoing description with the same care: no local candidate unless
/// `keep_local`, no related address, no private address in `c=` or `a=rtcp` lines.
pub fn scrub_sdp(sdp: &str, keep_local: bool) -> String {
    let newline = if sdp.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines = Vec::new();
    for line in sdp.split(newline) {
        if let Some(candidate) = line.strip_prefix("a=") {
            if candidate.starts_with("candidate:") {
                if !keep_local && classify(candidate) == CandidateKind::Local {
                    continue;
                }
                lines.push(format!("a={}", scrub_related(candidate)));
                continue;
            }
        }
        if !keep_local && (line.starts_with("c=") || line.starts_with("a=rtcp:")) {
            lines.push(blank_local_address(line));
            continue;
        }
        lines.push(line.to_string());
    }
    lines.join(newline)
}

/// `c=IN IP4 <addr>` / `a=rtcp:<port> IN IP4 <addr>` with a local `<addr>` unspecified.
fn blank_local_address(line: &str) -> String {
    let Some((head, address)) = line.rsplit_once(' ') else {
        return line.to_string();
    };
    match address.parse::<IpAddr>() {
        Ok(ip @ IpAddr::V4(_)) if is_local_address(&ip) => format!("{head} 0.0.0.0"),
        Ok(ip @ IpAddr::V6(_)) if is_local_address(&ip) => format!("{head} ::"),
        _ => line.to_string(),
    }
}

/// One connection's candidates on their way out: those with a local address wait until
/// the remote shows our own public address (same network), or until it is clear we have
/// none. `forced` (tests): `Some(true)` releases at once, `Some(false)` never.
#[derive(Debug, Default)]
pub struct AddressGate {
    forced: Option<bool>,
    held: Vec<IceCandidateData>,
    own: HashSet<IpAddr>,
    remote: HashSet<IpAddr>,
    released: bool,
    gathered: bool,
    timed_out: bool,
}

impl AddressGate {
    pub fn new(forced: Option<bool>) -> Self {
        Self { forced, ..Default::default() }
    }

    /// Whether private candidates (and addresses in SDP) go to this remote.
    pub fn released(&self) -> bool {
        self.released
    }

    /// A batch of our candidates: what to send now (local ones are held, related
    /// addresses scrubbed), plus any held ones this batch releases.
    pub fn local(&mut self, batch: Vec<IceCandidateData>) -> Vec<IceCandidateData> {
        let mut out = Vec::new();
        for mut candidate in batch {
            match classify(&candidate.candidate) {
                CandidateKind::Local if !self.released => {
                    self.held.push(candidate);
                    continue;
                }
                CandidateKind::Public(ip) => {
                    self.own.insert(ip);
                }
                _ => {}
            }
            candidate.candidate = scrub_related(&candidate.candidate);
            out.push(candidate);
        }
        out.extend(self.try_release());
        out
    }

    /// The remote's candidates show its public address: held ones released by it.
    pub fn remote(&mut self, batch: &[IceCandidateData]) -> Vec<IceCandidateData> {
        for candidate in batch {
            if let CandidateKind::Public(ip) = classify(&candidate.candidate) {
                self.remote.insert(ip);
            }
        }
        self.try_release()
    }

    /// Our gathering ended: without a public address, the held candidates go.
    pub fn gathering_done(&mut self) -> Vec<IceCandidateData> {
        self.gathered = true;
        self.try_release()
    }

    /// `RELEASE_WITHOUT_PUBLIC_MS` after our first candidate.
    pub fn timeout(&mut self) -> Vec<IceCandidateData> {
        self.timed_out = true;
        self.try_release()
    }

    fn try_release(&mut self) -> Vec<IceCandidateData> {
        if !self.released {
            self.released = match self.forced {
                Some(forced) => forced,
                None => {
                    self.own.intersection(&self.remote).next().is_some()
                        || (self.own.is_empty() && (self.gathered || self.timed_out))
                }
            };
        }
        if !self.released {
            return Vec::new();
        }
        std::mem::take(&mut self.held)
            .into_iter()
            .map(|mut c| {
                c.candidate = scrub_related(&c.candidate);
                c
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(candidate: &str) -> IceCandidateData {
        IceCandidateData { candidate: candidate.into(), sdp_mid: Some("0".into()), sdp_m_line_index: Some(0) }
    }

    const HOST_LAN: &str = "candidate:1 1 udp 2122260223 192.168.1.20 50000 typ host generation 0";
    const HOST_MDNS: &str = "candidate:2 1 udp 2122260223 3b1e2f-aa.local 50001 typ host generation 0";
    const HOST_V6: &str = "candidate:3 1 udp 2122197247 2001:db8::20 50002 typ host generation 0";
    const HOST_ULA: &str = "candidate:4 1 udp 2122197247 fd12:3456::20 50003 typ host generation 0";
    const SRFLX: &str = "candidate:5 1 udp 1686052607 203.0.113.7 61000 typ srflx raddr 192.168.1.20 rport 50000 generation 0";
    const RELAY: &str = "candidate:6 1 udp 41885439 198.51.100.9 3478 typ relay raddr 203.0.113.7 rport 61000 generation 0";
    const REMOTE_SAME: &str = "candidate:7 1 udp 1686052607 203.0.113.7 62000 typ srflx raddr 0.0.0.0 rport 0 generation 0";
    const REMOTE_OTHER: &str = "candidate:8 1 udp 1686052607 198.51.100.44 62000 typ srflx raddr 0.0.0.0 rport 0 generation 0";

    #[test]
    fn test_local_addresses() {
        for ip in ["10.1.2.3", "172.16.0.1", "172.31.255.1", "192.168.0.1", "100.64.0.1", "100.127.1.1", "169.254.1.1", "fd00::1", "fe80::1", "::ffff:192.168.1.1"] {
            assert!(is_local_address(&ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "172.32.0.1", "100.128.0.1", "127.0.0.1", "2001:db8::1", "::1"] {
            assert!(!is_local_address(&ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn test_classify() {
        assert_eq!(classify(HOST_LAN), CandidateKind::Local);
        assert_eq!(classify(HOST_ULA), CandidateKind::Local);
        assert_eq!(classify(HOST_MDNS), CandidateKind::Other);
        assert_eq!(classify(HOST_V6), CandidateKind::Public("2001:db8::20".parse().unwrap()));
        assert_eq!(classify(SRFLX), CandidateKind::Public("203.0.113.7".parse().unwrap()));
        assert_eq!(classify(RELAY), CandidateKind::Other);
        // A STUN or TURN server on the LAN reflects our LAN address: still ours, still held.
        assert_eq!(classify("candidate:5 1 udp 1686052607 192.168.3.14 35745 typ srflx raddr 0.0.0.0 rport 0"), CandidateKind::Local);
        assert_eq!(classify("candidate:5 1 udp 1 10.0.0.9 35745 typ prflx"), CandidateKind::Local);
        // A TURN server's relay address on the LAN is the server's, not ours.
        assert_eq!(classify("candidate:6 1 udp 1 10.0.0.1 3478 typ relay raddr 0.0.0.0 rport 0"), CandidateKind::Other);
        assert_eq!(classify("candidate:1 1 udp 1 127.0.0.1 3478 typ relay"), CandidateKind::Other);
        assert_eq!(classify("garbage"), CandidateKind::Other);
    }

    #[test]
    fn test_related_addresses_are_scrubbed() {
        assert_eq!(scrub_related(SRFLX), "candidate:5 1 udp 1686052607 203.0.113.7 61000 typ srflx raddr 0.0.0.0 rport 0 generation 0");
        assert!(scrub_related(RELAY).contains("raddr 0.0.0.0 rport 0"));
        let v6 = "candidate:9 1 udp 1 2001:db8::7 61000 typ srflx raddr fd00::2 rport 5";
        assert_eq!(scrub_related(v6), "candidate:9 1 udp 1 2001:db8::7 61000 typ srflx raddr :: rport 0");
        assert_eq!(scrub_related(HOST_LAN), HOST_LAN);
    }

    #[test]
    fn test_private_candidates_go_only_to_the_same_network() {
        let mut gate = AddressGate::new(None);
        let sent = gate.local(vec![c(HOST_LAN), c(HOST_MDNS), c(HOST_V6), c(SRFLX)]);
        let sent: Vec<&str> = sent.iter().map(|c| c.candidate.as_str()).collect();
        assert_eq!(sent.len(), 3, "the LAN address is held");
        assert!(sent.iter().all(|c| !c.contains("192.168.")));
        assert!(gate.remote(&[c(REMOTE_OTHER)]).is_empty(), "another network");
        assert!(!gate.released());
        let released = gate.remote(&[c(REMOTE_SAME)]);
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].candidate, HOST_LAN);
        assert!(gate.released());
        // Later ones go at once.
        assert_eq!(gate.local(vec![c(HOST_ULA)]).len(), 1);
    }

    #[test]
    fn test_without_a_public_address_they_go_as_before() {
        let mut gate = AddressGate::new(None);
        assert!(gate.local(vec![c(HOST_LAN)]).is_empty());
        assert_eq!(gate.gathering_done().len(), 1, "offline LAN: nothing to compare");
        let mut gate = AddressGate::new(None);
        assert!(gate.local(vec![c(HOST_LAN)]).is_empty());
        assert_eq!(gate.timeout().len(), 1, "STUN slow or blocked");
        // With a public address of our own, gathering ending changes nothing.
        let mut gate = AddressGate::new(None);
        gate.local(vec![c(HOST_LAN), c(SRFLX)]);
        assert!(gate.gathering_done().is_empty());
        assert!(gate.timeout().is_empty());
        assert!(!gate.released());
    }

    #[test]
    fn test_forced_decisions() {
        let mut never = AddressGate::new(Some(false));
        assert!(never.local(vec![c(HOST_LAN)]).is_empty());
        assert!(never.gathering_done().is_empty());
        assert!(never.remote(&[c(REMOTE_SAME)]).is_empty());
        let mut always = AddressGate::new(Some(true));
        assert_eq!(always.local(vec![c(HOST_LAN)]).len(), 1);
    }

    #[test]
    fn test_sdp_keeps_no_private_address() {
        let sdp = ["v=0", "c=IN IP4 192.168.1.20", "a=rtcp:9 IN IP4 10.0.0.2", &format!("a={HOST_LAN}"), &format!("a={SRFLX}"), "a=mid:0", ""].join("\r\n");
        let scrubbed = scrub_sdp(&sdp, false);
        assert!(!scrubbed.contains("192.168.") && !scrubbed.contains("10.0.0.2"), "{scrubbed}");
        assert!(scrubbed.contains("c=IN IP4 0.0.0.0\r\na=rtcp:9 IN IP4 0.0.0.0\r\n"));
        assert!(scrubbed.contains("typ srflx raddr 0.0.0.0 rport 0"));
        assert!(scrubbed.ends_with("a=mid:0\r\n"));
        let kept = scrub_sdp(&sdp, true);
        assert!(kept.contains(HOST_LAN) && kept.contains("c=IN IP4 192.168.1.20"));
        assert!(!kept.contains("raddr 192.168"));
        let public = "v=0\nc=IN IP4 203.0.113.7\n";
        assert_eq!(scrub_sdp(public, false), public);
    }
}
