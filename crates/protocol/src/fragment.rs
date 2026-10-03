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
