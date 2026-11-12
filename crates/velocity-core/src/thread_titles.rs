//! Once-only thread title selection (SPEC.md F4).
//!
//! The caller owns provider requests, timers and worktree setup. Generic title
//! values keep this policy independent of transport and persisted thread data.

/// A late response must not rename the branch after the fallback was selected.
pub const TITLE_DEADLINE_MILLISECONDS: u64 = 2_000;

#[derive(Debug, PartialEq, Eq)]
pub enum TitleSource<Title> {
    Message(Title),
    Item(Title),
}

#[derive(Debug, PartialEq, Eq)]
enum Selection<Title> {
    Pending,
    Fallback,
    Model(Title),
}

/// Once settled, the selected value survives failures and late model callbacks.
#[derive(Debug, PartialEq, Eq)]
pub struct ThreadTitle<Title> {
    fallback: Title,
    selection: Selection<Title>,
}

impl<Title> ThreadTitle<Title> {
    /// Item-backed launches never request a title model.
    pub fn new(source: TitleSource<Title>, model_available: bool) -> Self {
        match source {
            TitleSource::Message(first_message) => Self {
                fallback: first_message,
                selection: if model_available {
                    Selection::Pending
                } else {
                    Selection::Fallback
                },
            },
            TitleSource::Item(title) => Self {
                fallback: title,
                selection: Selection::Fallback,
            },
        }
    }

    /// Only a new pending title needs a model request; settling never retries it.
    pub fn is_pending(&self) -> bool {
        matches!(self.selection, Selection::Pending)
    }

    /// Pass `None` on failure or when the two-second deadline expires. Elapsed
    /// time also rejects late responses when a delayed timer has not fired yet.
    pub fn settle(self, response: Option<Title>, elapsed_milliseconds: u64) -> Self {
        if !self.is_pending() {
            return self;
        }
        let selection = if elapsed_milliseconds <= TITLE_DEADLINE_MILLISECONDS {
            match response {
                Some(title) => Selection::Model(title),
                None => Selection::Fallback,
            }
        } else {
            Selection::Fallback
        };
        Self {
            fallback: self.fallback,
            selection,
        }
    }

    /// Pending titles display the first message until selection settles.
    pub fn displayed_title(&self) -> &Title {
        match &self.selection {
            Selection::Model(title) => title,
            Selection::Pending | Selection::Fallback => &self.fallback,
        }
    }

    /// Naming must wait for this value, so a late model cannot rename a running
    /// thread's branch or move its worktree.
    pub fn final_title(&self) -> Option<&Title> {
        if self.is_pending() {
            None
        } else {
            Some(self.displayed_title())
        }
    }

    /// Readiness is supplied by the worktree manager only after successful setup
    /// and application of the final title-derived branch and directory names.
    pub fn can_start(&self, worktree_ready: bool, name_applied: bool) -> bool {
        !self.is_pending() && worktree_ready && name_applied
    }
}

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
        "Name this coding task as a short imperative title of at most six words, \
         like \"Wire thread list to Tauri IPC\". Reply with the title only: no quotes, \
         no punctuation at the end, no explanation.\n\nTask:\n{first_message}"
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
