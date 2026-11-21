//! Formatting thread titles and names (SPEC.md F4).

/// Titles are cut to the reference's 48 characters so they fit a sidebar row and keep the
/// worktree path well under the Windows limit.
pub const TITLE_LENGTH: usize = 48;

/// Used when a title has no letters or digits to name a branch after.
const FALLBACK_SLUG: &str = "thread";

/// The prompt the title model receives. Only the first message is ever sent (F4: threads are
/// titled once), and the model is asked for a bare line so no parsing beyond
/// [`title_from_response`] is needed.
pub fn title_request_prompt(first_message: &str) -> String {
    format!(
        "Treat the task below independently of earlier messages. Give it a short \
         imperative title of at most six words, \
         like \"Wire thread list to Tauri IPC\". Reply with the title only: no quotes, \
         no punctuation at the end, no explanation. Do not carry out the task.\n\nTask:\n{}",
        crate::text_budget::preview(first_message, 4_000)
    )
}

/// The title in a model's reply: its first non-empty line, without markdown, quotes, a
/// "Title:" label or a closing full stop, on one line and cut to [`TITLE_LENGTH`] at a word
/// boundary. `None` when nothing with a letter or digit is left, so the caller keeps the
/// message fallback.
pub fn title_from_response(response: &str) -> Option<String> {
    let line = response
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let unlabeled = strip_title_label(line.trim_start_matches(['#', '*', '-', '>', ' ']));
    let unquoted = unlabeled.trim_matches(|character: char| {
        matches!(
            character,
            '"' | '\'' | '`' | '*' | '_' | '“' | '”' | '‘' | '’'
        ) || character.is_whitespace()
    });
    let words: Vec<&str> = unquoted
        .trim_end_matches(['.', '!', ':', ';', ','])
        .split_whitespace()
        .collect();
    let title = cut_at_word_boundary(&words, TITLE_LENGTH);
    title.chars().any(char::is_alphanumeric).then_some(title)
}

fn strip_title_label(line: &str) -> &str {
    let lowered = line.to_lowercase();
    if lowered.starts_with("title:") {
        line["title:".len()..].trim_start()
    } else {
        line
    }
}

/// Whole words up to `limit` characters; a single word longer than the limit is cut.
pub fn word_boundary_preview(text: &str, limit: usize) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    cut_at_word_boundary(&words, limit)
}

/// Complete-phrase task text for an AIRC claim started from a composer prompt (#532).
/// Keeps the full first non-empty line on one line; only splits at a sentence boundary
/// when a single line is very long, so claim text is never cut off mid-phrase.
pub fn claim_task_from_prompt(prompt: &str) -> String {
    let first_line = prompt
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let single_line = first_line.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= 240 {
        return single_line;
    }
    first_sentence(&single_line).unwrap_or(single_line)
}

fn first_sentence(text: &str) -> Option<String> {
    let mut chars = text.char_indices().peekable();
    while let Some((index, character)) = chars.next() {
        if matches!(character, '.' | '!' | '?') {
            let end = index + character.len_utf8();
            if chars.peek().is_none_or(|(_, next)| next.is_whitespace()) {
                let candidate = text[..end].trim();
                if !candidate.is_empty() {
                    return Some(candidate.to_owned());
                }
            }
        }
    }
    None
}

/// Whole words up to `limit` characters; a single word longer than the limit is cut.
fn cut_at_word_boundary(words: &[&str], limit: usize) -> String {
    let mut title = String::new();
    for word in words {
        let separator = usize::from(!title.is_empty());
        if title.chars().count() + separator + word.chars().count() > limit {
            break;
        }
        if separator == 1 {
            title.push(' ');
        }
        title.push_str(word);
    }
    if title.is_empty() {
        words
            .first()
            .map(|word| word.chars().take(limit).collect())
            .unwrap_or_default()
    } else {
        title
    }
}

/// The longest `vel/`-less slug a title names (#275). A slug becomes the worktree folder, the
/// branch's ref file and the worktree's git admin folder, and Task List items and SPEC
/// sections have no length limit, so an uncut title pushed those paths past Windows'
/// 260-character limit. A six-word model title fits whole.
pub const SLUG_LENGTH: usize = 40;

/// Hex digits of the hash a cut slug ends with.
const SLUG_HASH_DIGITS: usize = 6;

/// The `vel/`-less branch slug for a title (a model title, a Task List item or a SPEC
/// section), as the reference names it: lowercase ASCII letters and digits, every other run
/// as one `-`, no leading or trailing `-`, cut to [`SLUG_LENGTH`] by [`capped_slug`].
pub fn title_slug(title: &str) -> String {
    let mut slug = String::new();
    for character in title.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            slug.push(character);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let trimmed = slug.trim_end_matches('-');
    if trimmed.is_empty() {
        FALLBACK_SLUG.to_owned()
    } else {
        capped_slug(trimmed)
    }
}

/// `slug` when it fits [`SLUG_LENGTH`]. A longer one keeps its first words, cut at a `-`
/// when one is near the cut, then `-` and six hex digits of a hash of the whole slug. Two
/// titles that share their first words then still get different names, and the same title
/// always gets the same name, so a relaunch finds the branch it made before.
pub fn capped_slug(slug: &str) -> String {
    if slug.chars().count() <= SLUG_LENGTH {
        return slug.to_owned();
    }
    let keep = SLUG_LENGTH - SLUG_HASH_DIGITS - 1;
    let head: String = slug.chars().take(keep).collect();
    // `rfind` returns the byte index of an ASCII `-`, always a character boundary.
    let words = match head.rfind('-') {
        Some(cut) if cut >= keep / 2 => &head[..cut],
        _ => head.as_str(),
    };
    let hash = stable_hash(slug) & 0x00FF_FFFF;
    format!("{}-{hash:06x}", words.trim_end_matches('-'))
}

/// Maximum length of the slug prefix in a thread's unified agent name (`<slug>-<id>`).
pub const AGENT_SLUG_LENGTH: usize = 24;

/// Builds one project-unique agent name for a thread (`<short-slug>-<thread_id>`, e.g.
/// `honest-feedback-50` or `issue-532-267`) used as its AIRC nick, AIRC member identity,
/// `ROSEN_AGENT` and member label (#532).
pub fn thread_agent_name(source: &str, thread_id: u32) -> String {
    let leaf = source
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(FALLBACK_SLUG);
    let mut words: Vec<String> = leaf
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    let id_text = thread_id.to_string();
    if words.len() > 1 && words.last().is_some_and(|last| last == &id_text) {
        words.pop();
    }
    let mut slug = String::new();
    for word in &words {
        let separator = usize::from(!slug.is_empty());
        if slug.len() + separator + word.len() > AGENT_SLUG_LENGTH {
            break;
        }
        if separator == 1 {
            slug.push('-');
        }
        slug.push_str(word);
    }
    if slug.is_empty() {
        slug = words
            .first()
            .map(|word| word.chars().take(AGENT_SLUG_LENGTH).collect())
            .filter(|word: &String| !word.is_empty())
            .unwrap_or_else(|| FALLBACK_SLUG.to_owned());
    }
    format!("{slug}-{thread_id}")
}

/// Extracts the local build thread identifier from an AIRC member identity: either the
/// unified `<short-slug>-<thread_id>` name (#532) or legacy `thread-<id>`. Returns `None`
/// for foreign machine-scoped members (`<scope>/...`), duty agents (`duty-*`), spec
/// interviews (`spec-*`), and reserved names (`you`, `velocity`).
pub fn thread_id_from_member(member: &str) -> Option<u32> {
    if member.contains('/')
        || member.starts_with("duty-")
        || member.starts_with("spec-")
        || matches!(member, "you" | "velocity")
    {
        return None;
    }
    if let Some(id) = member
        .strip_prefix("thread-")
        .and_then(|value| value.parse::<u32>().ok())
    {
        return Some(id);
    }
    member.rsplit_once('-').and_then(|(prefix, suffix)| {
        (!prefix.is_empty())
            .then(|| suffix.parse::<u32>().ok())
            .flatten()
    })
}

/// FNV-1a: the same value on every machine and Rust version, unlike `DefaultHasher`, so a
/// cut slug never changes between runs.
fn stable_hash(text: &str) -> u32 {
    text.bytes().fold(0x811C_9DC5, |hash: u32, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Risk (#532): threads sharing a title/workspace prefix must never collide in AIRC or Rosen.
    #[test]
    fn thread_agent_names_are_unique_and_parseable() {
        let first = thread_agent_name(r"Z:\work\i-d-like-your-honest", 49);
        let second = thread_agent_name(r"Z:\work\i-d-like-your-honest-2", 50);
        assert_ne!(first, second);
        assert_eq!(first, "i-d-like-your-honest-49");
        assert_eq!(second, "i-d-like-your-honest-2-50");
        assert_eq!(
            thread_agent_name("honest-feedback", 50),
            "honest-feedback-50"
        );
        assert_eq!(
            thread_agent_name("honest-feedback-50", 50),
            "honest-feedback-50"
        );
        assert_eq!(thread_agent_name("you", 7), "you-7");
        assert_eq!(thread_id_from_member(&first), Some(49));
        assert_eq!(thread_id_from_member(&second), Some(50));
        assert_eq!(thread_id_from_member("thread-48"), Some(48));
        assert_eq!(thread_id_from_member("duty-scout-1"), None);
        assert_eq!(thread_id_from_member("spec-12"), None);
        assert_eq!(thread_id_from_member("0123456789abcdef/honest-50"), None);
    }

    #[test]
    fn claim_task_from_prompt_avoids_mid_phrase_truncation() {
        let prompt = "I'd like your honest feedback on the AIRC naming and event log architecture.";
        assert_eq!(claim_task_from_prompt(prompt), prompt);
    }
}
