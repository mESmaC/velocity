//! Where a path in an `fs` or `terminal` request points (SPEC.md §3.2).
//!
//! Relative paths are resolved against the thread's worktree. Absolute paths are used as
//! given: the worktree is the agent's working directory, not a jail (§3.2 Tool access).
//! Paths are UTF-8 bytes; separators, drive letters and colons are ASCII, so every test here
//! is exact on UTF-8. `proofs/AcpClient/Design.lean` (`resolvePath`) proves that the result
//! on an absolute worktree is always absolute, so no relative path ever reaches the operating
//! system, where it would resolve against Velocity's own folder instead of the worktree.
//!
//! Paths over 260 characters need no special form here: Rust's standard library adds the
//! `\\?\` prefix itself when it opens a long path on Windows.

const BACKSLASH: u8 = b'\\';
const SLASH: u8 = b'/';
const COLON: u8 = b':';

/// The shapes of a requested path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathShape {
    Empty,
    /// `C:\…`, `C:/…`, `\\server\share\…`, `\\?\…`, `//…`: used as given.
    Absolute,
    /// `\…` or `/…`: from the root of the worktree's drive or share. On a POSIX worktree it
    /// is an absolute path.
    RootRelative,
    /// `C:…` with no separator after the colon: relative to the current folder of drive
    /// `C:`, which is the worktree when the worktree is on `C:` and the drive's root
    /// otherwise.
    DriveRelative,
    /// Anything else: relative to the worktree.
    Relative,
}

/// `\` and `/`: Windows accepts both.
pub fn is_separator(byte: u8) -> bool {
    byte == BACKSLASH || byte == SLASH
}

fn is_between(byte: u8, low: u8, high: u8) -> bool {
    if low <= byte { byte <= high } else { false }
}

pub fn is_drive_letter(byte: u8) -> bool {
    is_between(byte, b'A', b'Z') || is_between(byte, b'a', b'z')
}

/// Drive letters compare without case: `c:` is `C:`.
fn fold_case(byte: u8) -> u8 {
    if is_between(byte, b'A', b'Z') {
        byte + 32
    } else {
        byte
    }
}

fn to_backslash(byte: u8) -> u8 {
    if byte == SLASH { BACKSLASH } else { byte }
}

pub fn shape(path: &[u8]) -> PathShape {
    if path.is_empty() {
        PathShape::Empty
    } else if path.len() == 1 {
        if is_separator(path[0]) {
            PathShape::RootRelative
        } else {
            PathShape::Relative
        }
    } else if is_separator(path[0]) {
        if is_separator(path[1]) {
            PathShape::Absolute
        } else {
            PathShape::RootRelative
        }
    } else if is_drive_letter(path[0]) && path[1] == COLON {
        if path.len() == 2 {
            PathShape::DriveRelative
        } else if is_separator(path[2]) {
            PathShape::Absolute
        } else {
            PathShape::DriveRelative
        }
    } else {
        PathShape::Relative
    }
}

/// The worktree's drive letter, case folded.
fn drive_of(worktree: &[u8]) -> Option<u8> {
    if worktree.len() >= 2 && is_drive_letter(worktree[0]) && worktree[1] == COLON {
        Some(fold_case(worktree[0]))
    } else {
        None
    }
}

/// Windows worktrees get `\` separators.
fn windows_style(worktree: &[u8]) -> bool {
    match drive_of(worktree) {
        Some(_) => true,
        None => worktree.contains(&BACKSLASH),
    }
}

/// Where the share name ends in `\\server\share\…`: the second separator after the leading
/// `\\`, or the end of the path.
fn unc_root_end(worktree: &[u8]) -> usize {
    let mut separators = 0;
    let mut index = 2;
    while index < worktree.len() {
        if is_separator(worktree[index]) {
            if separators == 1 {
                return index;
            }
            separators += 1;
        }
        index += 1;
    }
    index
}

/// The length of the worktree's root: `C:` (2), `\\server\share`, or nothing on POSIX.
fn root_length(worktree: &[u8]) -> usize {
    if worktree.len() < 2 {
        0
    } else if is_drive_letter(worktree[0]) && worktree[1] == COLON {
        2
    } else if is_separator(worktree[0]) && is_separator(worktree[1]) {
        unc_root_end(worktree)
    } else {
        0
    }
}

fn needs_separator(worktree: &[u8]) -> bool {
    if worktree.is_empty() {
        false
    } else {
        !is_separator(worktree[worktree.len() - 1])
    }
}

fn separator_for(worktree: &[u8]) -> u8 {
    if windows_style(worktree) {
        BACKSLASH
    } else {
        SLASH
    }
}

/// Appends `bytes[start..end]`, turning `/` into `\` when `windows` is set.
fn push_bytes(out: &mut Vec<u8>, bytes: &[u8], start: usize, end: usize, windows: bool) {
    let mut index = start;
    while index < end {
        let byte = bytes[index];
        out.push(if windows { to_backslash(byte) } else { byte });
        index += 1;
    }
}

/// The worktree, one separator, then `part[start..]`.
fn join_relative(worktree: &[u8], part: &[u8], start: usize) -> Vec<u8> {
    let mut out = worktree.to_vec();
    if needs_separator(worktree) {
        out.push(separator_for(worktree));
    }
    push_bytes(&mut out, part, start, part.len(), windows_style(worktree));
    out
}

fn same_drive(drive: Option<u8>, letter: u8) -> bool {
    match drive {
        Some(drive) => drive == fold_case(letter),
        None => false,
    }
}

/// Where `requested` points, for an agent working in `worktree`.
pub fn resolve_path(worktree: &[u8], requested: &[u8]) -> Vec<u8> {
    match shape(requested) {
        PathShape::Empty => worktree.to_vec(),
        PathShape::Absolute => requested.to_vec(),
        PathShape::RootRelative => {
            let mut out = Vec::new();
            push_bytes(&mut out, worktree, 0, root_length(worktree), false);
            push_bytes(
                &mut out,
                requested,
                0,
                requested.len(),
                windows_style(worktree),
            );
            out
        }
        PathShape::DriveRelative => {
            let drive = requested[0];
            if same_drive(drive_of(worktree), drive) {
                join_relative(worktree, requested, 2)
            } else {
                let mut out = Vec::from([drive, COLON, BACKSLASH]);
                push_bytes(&mut out, requested, 2, requested.len(), true);
                out
            }
        }
        PathShape::Relative => join_relative(worktree, requested, 0),
    }
}
