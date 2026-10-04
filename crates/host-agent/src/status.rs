//! What the terminal shows. Names come from other people: strip anything that could
//! rewrite the terminal (escape sequences) or reorder text (bidi controls).

const MAX_NAME_CHARS: usize = 40;

pub fn sanitize_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(*c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
        })
        .take(MAX_NAME_CHARS)
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "someone".into()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_names_cannot_control_the_terminal() {
        assert_eq!(sanitize_name("Bo · 3fa2"), "Bo · 3fa2");
        assert_eq!(sanitize_name("\u{1b}[2J\u{1b}]0;pwned\u{7}Eve"), "[2J]0;pwnedEve");
        assert_eq!(sanitize_name("\u{202E}evE"), "evE");
        assert_eq!(sanitize_name("   "), "someone");
        assert_eq!(sanitize_name(&"x".repeat(100)).len(), MAX_NAME_CHARS);
    }
}
