use crate::crypto::key_from_base64;

/// Ordered key/value pairs of a URL fragment (`#room=..&key=..&relays=..`).
///
/// Round-trips every parameter, including ones this version does not know about,
/// so rewriting one value never silently drops the others.
/// Invariant: fragments carry secrets and are never sent to any server.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FragmentParams {
    pairs: Vec<(String, Option<String>)>,
}

impl FragmentParams {
    /// Parse a fragment with or without the leading `#`. Values are kept verbatim
    /// (no percent-decoding) and only the first `=` separates key from value.
    pub fn parse(hash: &str) -> Self {
        let query = hash.strip_prefix('#').unwrap_or(hash);
        let pairs = query
            .split('&')
            .filter(|segment| !segment.is_empty())
            .map(|segment| match segment.split_once('=') {
                Some((k, v)) => (k.to_string(), Some(v.to_string())),
                None => (segment.to_string(), None),
            })
            .collect();
        Self { pairs }
    }

    /// The room in a pasted or launched dchat link: everything after the first `#`, from any
    /// site's link or a bare `#room=..&key=..`. Only the fragment is kept (the room lives
    /// entirely in it); `None` unless it names a room with a valid key.
    pub fn from_link(link: &str) -> Option<Self> {
        let (_, hash) = link.trim().split_once('#')?;
        let params = Self::parse(hash);
        params.get("room")?;
        key_from_base64(params.get("key")?).ok()?;
        Some(params)
    }

    /// First non-empty value for `key`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.pairs
            .iter()
            .filter(|(k, _)| k == key)
            .find_map(|(_, v)| v.as_deref().filter(|v| !v.is_empty()))
    }

    /// Replace the first occurrence of `key` in place (dropping duplicates), or append it.
    pub fn set(&mut self, key: &str, value: &str) {
        match self.pairs.iter().position(|(k, _)| k == key) {
            Some(idx) => {
                self.pairs[idx].1 = Some(value.to_string());
                let mut seen = false;
                self.pairs.retain(|(k, _)| {
                    if k != key {
                        return true;
                    }
                    let keep = !seen;
                    seen = true;
                    keep
                });
            }
            None => self.pairs.push((key.to_string(), Some(value.to_string()))),
        }
    }

    pub fn remove(&mut self, key: &str) {
        self.pairs.retain(|(k, _)| k != key);
    }

    /// Serialize back to `#k=v&...`, or an empty string when there are no parameters.
    pub fn to_hash(&self) -> String {
        if self.pairs.is_empty() {
            return String::new();
        }
        let body = self
            .pairs
            .iter()
            .map(|(k, v)| match v {
                Some(v) => format!("{k}={v}"),
                None => k.clone(),
            })
            .collect::<Vec<_>>()
            .join("&");
        format!("#{body}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip_preserves_unknown_params() {
        let hash = "#room=ab12cd34&key=Zm9v_YmFy-&relays=wss://a.example,wss://b.example&future=1&flag";
        let params = FragmentParams::parse(hash);
        assert_eq!(params.get("room"), Some("ab12cd34"));
        assert_eq!(params.get("key"), Some("Zm9v_YmFy-"));
        assert_eq!(params.get("relays"), Some("wss://a.example,wss://b.example"));
        assert_eq!(params.get("future"), Some("1"));
        assert_eq!(params.get("flag"), None);
        assert_eq!(params.to_hash(), hash);
    }

    #[test]
    fn test_set_replaces_in_place_and_appends() {
        let mut params = FragmentParams::parse("#relays=wss://r.example&key=old&key=dup");
        params.set("key", "new");
        params.set("room", "r1");
        assert_eq!(params.to_hash(), "#relays=wss://r.example&key=new&room=r1");
    }

    #[test]
    fn test_only_first_equals_splits() {
        let params = FragmentParams::parse("relays=wss://r.example/?a=b");
        assert_eq!(params.get("relays"), Some("wss://r.example/?a=b"));
    }

    const KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";

    #[test]
    fn test_from_link_keeps_only_the_fragment() {
        let link = format!("https://mirror.example/dchat/?q=1#room=r1&key={KEY}&relays=nostr&pw=salt&admsk=ab&future=1");
        let params = FragmentParams::from_link(&link).unwrap();
        assert_eq!(params.to_hash(), format!("#room=r1&key={KEY}&relays=nostr&pw=salt&admsk=ab&future=1"));
        let bare = FragmentParams::from_link(&format!("  #room=r1&key={KEY}\n")).unwrap();
        assert_eq!(bare.to_hash(), format!("#room=r1&key={KEY}"));
    }

    #[test]
    fn test_from_link_rejects_links_without_a_room() {
        assert_eq!(FragmentParams::from_link(""), None);
        assert_eq!(FragmentParams::from_link("https://dchat.example/"), None);
        assert_eq!(FragmentParams::from_link(&format!("room=r1&key={KEY}")), None);
        assert_eq!(FragmentParams::from_link(&format!("#key={KEY}")), None);
        assert_eq!(FragmentParams::from_link("#room=r1"), None);
        assert_eq!(FragmentParams::from_link("#room=r1&key=c2hvcnQ"), None);
        assert_eq!(FragmentParams::from_link("#room=r1&key=not base64!"), None);
    }

    #[test]
    fn test_empty_and_remove() {
        assert_eq!(FragmentParams::parse("").to_hash(), "");
        assert_eq!(FragmentParams::parse("#").to_hash(), "");
        let mut params = FragmentParams::parse("#room=&key=k&&x=1");
        assert_eq!(params.get("room"), None);
        params.remove("x");
        assert_eq!(params.to_hash(), "#room=&key=k");
    }
}
