//! Session display names. Kept in RAM only and re-asked after a reload.

const ADJECTIVES: [&str; 24] = [
    "Teal", "Amber", "Coral", "Jade", "Ivory", "Scarlet", "Indigo", "Olive", "Silver", "Golden",
    "Crimson", "Azure", "Violet", "Copper", "Misty", "Sunny", "Quiet", "Swift", "Brave", "Clever",
    "Gentle", "Lucky", "Merry", "Witty",
];

const ANIMALS: [&str; 24] = [
    "Otter", "Fox", "Heron", "Lynx", "Panda", "Falcon", "Badger", "Koala", "Raven", "Gecko",
    "Walrus", "Bison", "Ibis", "Marten", "Puffin", "Tapir", "Wombat", "Yak", "Moose", "Owl",
    "Seal", "Crane", "Hare", "Newt",
];

pub const MAX_NAME_CHARS: usize = 32;

/// A random two-word name such as "Teal Otter".
pub fn random_name() -> String {
    let pick = |len: usize| (js_sys::Math::random() * len as f64) as usize % len;
    format!("{} {}", ADJECTIVES[pick(ADJECTIVES.len())], ANIMALS[pick(ANIMALS.len())])
}

/// Trim, collapse whitespace and cap the length; an empty result gets a random name.
pub fn sanitize_name(raw: &str) -> String {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let capped: String = collapsed.chars().filter(|c| !c.is_control()).take(MAX_NAME_CHARS).collect();
    if capped.is_empty() {
        random_name()
    } else {
        capped
    }
}

/// Short public-key tag shown next to names so two people with the same name stay distinct.
pub fn pubkey_tag(pubkey: &str) -> String {
    pubkey.chars().take(4).collect()
}
