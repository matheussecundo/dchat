//! Which Nostr relays a room uses. Chosen by the room creator and carried in the link
//! (`&relays=`), since every member must use the same relays to find each other.

/// Used when a room link names no relays (on a public host).
pub const PUBLIC_RELAYS: [&str; 3] = ["wss://relay.damus.io", "wss://nos.lol", "wss://relay.primal.net"];

/// In `&relays=`, this entry stands for all of `PUBLIC_RELAYS`.
pub const PUBLIC_RELAYS_KEYWORD: &str = "nostr";

/// More relays only add duplicate traffic.
pub const MAX_RELAYS: usize = 8;

/// A `ws://` or `wss://` URL with a host and nothing that could break the fragment.
pub fn is_relay_url(value: &str) -> bool {
    let rest = value
        .strip_prefix("wss://")
        .or_else(|| value.strip_prefix("ws://"));
    rest.is_some_and(|r| {
        let host = r.split(['/', '?']).next().unwrap_or("");
        !host.is_empty()
            && value.len() <= 200
            && !value.chars().any(|c| c.is_whitespace() || c == ',' || c == '&' || c == '#')
    })
}

/// The relays named by a `&relays=` value, exactly: `nostr` expands to the public relays,
/// invalid entries are dropped, duplicates removed, at most `MAX_RELAYS`.
pub fn parse_relay_list(value: &str) -> Vec<String> {
    let mut relays: Vec<String> = Vec::new();
    for entry in value.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let expanded: Vec<String> = if entry.eq_ignore_ascii_case(PUBLIC_RELAYS_KEYWORD) {
            PUBLIC_RELAYS.iter().map(|r| r.to_string()).collect()
        } else if is_relay_url(entry) {
            vec![entry.trim_end_matches('/').to_string()]
        } else {
            Vec::new()
        };
        for relay in expanded {
            if !relays.contains(&relay) && relays.len() < MAX_RELAYS {
                relays.push(relay);
            }
        }
    }
    relays
}

/// Split what a creator typed ("wss://a, wss://b") into valid relay URLs and rejected entries.
pub fn split_relay_input(input: &str) -> (Vec<String>, Vec<String>) {
    let mut valid = Vec::new();
    let mut invalid = Vec::new();
    for entry in input.split([',', '\n', ' ']).map(str::trim).filter(|e| !e.is_empty()) {
        if is_relay_url(entry) {
            let url = entry.trim_end_matches('/').to_string();
            if !valid.contains(&url) {
                valid.push(url);
            }
        } else {
            invalid.push(entry.to_string());
        }
    }
    (valid, invalid)
}

/// The `&relays=` value for the creator's choice: their relays, plus the public ones as
/// backup when `include_public`.
pub fn format_relay_list(custom: &[String], include_public: bool) -> String {
    let mut parts: Vec<&str> = custom.iter().map(String::as_str).collect();
    if include_public {
        parts.push(PUBLIC_RELAYS_KEYWORD);
    }
    parts.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_relay_url_validation() {
        assert!(is_relay_url("wss://relay.example.com"));
        assert!(is_relay_url("wss://relay.example.com:7447/path"));
        assert!(is_relay_url("ws://127.0.0.1:3333/nostr"));
        assert!(!is_relay_url("https://relay.example.com"));
        assert!(!is_relay_url("wss://"));
        assert!(!is_relay_url("wss:///nohost"));
        assert!(!is_relay_url("wss://a.example&key=x"));
        assert!(!is_relay_url("wss://a b"));
        assert!(!is_relay_url(&format!("wss://{}", "a".repeat(300))));
    }

    #[test]
    fn test_parse_is_exact_and_expands_keyword() {
        assert_eq!(parse_relay_list("wss://mine.example"), vec!["wss://mine.example"]);
        assert_eq!(
            parse_relay_list("wss://mine.example/,nostr"),
            vec!["wss://mine.example", "wss://relay.damus.io", "wss://nos.lol", "wss://relay.primal.net"]
        );
        assert_eq!(parse_relay_list("NOSTR"), PUBLIC_RELAYS.to_vec());
        assert_eq!(parse_relay_list("wss://a.example,wss://a.example,bogus,,"), vec!["wss://a.example"]);
        assert!(parse_relay_list("bogus,https://x.example").is_empty());
        let many = (0..20).map(|i| format!("wss://r{i}.example")).collect::<Vec<_>>().join(",");
        assert_eq!(parse_relay_list(&many).len(), MAX_RELAYS);
    }

    #[test]
    fn test_input_split_and_format_roundtrip() {
        let (valid, invalid) = split_relay_input(" wss://a.example, wss://b.example/\nnot-a-url ");
        assert_eq!(valid, vec!["wss://a.example", "wss://b.example"]);
        assert_eq!(invalid, vec!["not-a-url"]);
        let value = format_relay_list(&valid, true);
        assert_eq!(value, "wss://a.example,wss://b.example,nostr");
        assert_eq!(parse_relay_list(&value).len(), 5);
        assert_eq!(format_relay_list(&valid, false), "wss://a.example,wss://b.example");
    }
}
