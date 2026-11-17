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
         no punctuation at the end, no explanation. Do not carry out the task.\n\nTask:\n{first_message}"
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

/// FNV-1a: the same value on every machine and Rust version, unlike `DefaultHasher`, so a
/// cut slug never changes between runs.
fn stable_hash(text: &str) -> u32 {
    text.bytes().fold(0x811C_9DC5, |hash: u32, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    })
}
