//! A new thread's title and branch before the title model names it (SPEC.md F4).
//!
//! F4 keeps "the slug of your message" when the title model fails or is slow; until thread
//! titles land (#14) that fallback is the only name. The rules follow the reference's
//! `send()`: the title is the message's first 48 characters, the branch is `vel/` plus the
//! message lowercased, with every run of other characters turned into `-`, cut to 20.

const TITLE_LENGTH: usize = 48;
const SLUG_LENGTH: usize = 20;
/// Used when a message has no letters or digits to name a branch after.
const FALLBACK_SLUG: &str = "thread";

/// The message's first 48 characters, on one line.
pub fn fallback_title(message: &str) -> String {
    let single_line = message.split_whitespace().collect::<Vec<_>>().join(" ");
    single_line.chars().take(TITLE_LENGTH).collect()
}

/// `vel/`-less branch slug from the message: lowercase ASCII letters and digits, other runs
/// as one `-`, at most 20 characters, no leading or trailing `-`.
pub fn message_slug(message: &str) -> String {
    let mut slug = String::new();
    for character in message.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            slug.push(character);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let cut: String = slug.chars().take(SLUG_LENGTH).collect();
    let trimmed = cut.trim_matches('-');
    if trimmed.is_empty() {
        FALLBACK_SLUG.to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// `slug`, or `slug-2`, `slug-3`… for the first one `is_taken` doesn't reject. Worktree
/// folders and branches are unique per project (F5 Guards), and a second message with the
/// same opening words must not reuse the first thread's branch, even one whose branch and
/// worktree are gone: a saved thread keeps its branch identity (#56).
///
/// Only finitely many names are ever taken, so the search ends one past them. `None` only
/// if every suffix is taken; callers refuse then, rather than reuse a taken name.
pub fn unique_slug(slug: &str, is_taken: impl Fn(&str) -> bool) -> Option<String> {
    if !is_taken(slug) {
        return Some(slug.to_owned());
    }
    (2_u32..=u32::MAX)
        .map(|suffix| format!("{slug}-{suffix}"))
        .find(|candidate| !is_taken(candidate))
}
