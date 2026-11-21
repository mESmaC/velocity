//! A project's root spec and Markdown files recursively under `spec/`, read as one spec.

mod sections;

pub use sections::{
    ImplementationItem, OpenQuestion, SectionSpan, TickQuery, extract_acceptance_criteria,
    extract_section_ids, tick_implementation_item,
};

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const ROOT_FILE: &str = "SPEC.md";
pub const FOLDER: &str = "spec";

/// One source file. Keeping boundaries prevents a section in one file consuming another.
pub struct SpecFile {
    pub path: PathBuf,
    pub markdown: String,
}

/// The root file first, followed by folder files in repository-relative path order.
pub struct ProjectSpec {
    pub files: Vec<SpecFile>,
}

impl ProjectSpec {
    /// `root_file` is the saved `spec_path`; old projects keep their configured root file.
    /// A missing root is allowed when the folder supplies the spec. Other errors fail the
    /// entire read, so unreadable rules never silently disappear from the project.
    pub fn read(checkout: &Path, root_file: &Path) -> io::Result<Self> {
        // Canonical checkout paths carry Windows' extended prefix, including for UNC paths.
        let checkout = fs::canonicalize(checkout)?;
        let root = checkout.join(root_file);
        let mut files = Vec::new();
        let missing = match fs::read_to_string(&root) {
            Ok(markdown) => {
                files.push(SpecFile {
                    path: root_file.to_owned(),
                    markdown,
                });
                None
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Some(error),
            Err(error) => return Err(error),
        };
        let mut paths = markdown_paths(&checkout.join(FOLDER))?;
        paths.sort();
        let root = fs::canonicalize(&root).ok();
        for path in paths {
            if root.as_ref().is_some_and(|root| root == &path) {
                continue;
            }
            let markdown = fs::read_to_string(&path)?;
            let relative = path.strip_prefix(&checkout).map_err(io::Error::other)?;
            files.push(SpecFile {
                path: relative.to_owned(),
                markdown,
            });
        }
        if files.is_empty() {
            return Err(missing
                .unwrap_or_else(|| io::Error::new(io::ErrorKind::NotFound, "No project spec")));
        }
        Ok(Self { files })
    }

    pub fn read_default(checkout: &Path) -> io::Result<Self> {
        Self::read(checkout, Path::new(ROOT_FILE))
    }

    /// The original bytes for a single-file project; blank lines separate multiple files.
    pub fn markdown(&self) -> String {
        self.files
            .iter()
            .map(|file| file.markdown.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub fn has_section(&self, identifier: &str) -> bool {
        self.files.iter().any(|file| {
            headings(&file.markdown)
                .any(|(_, title)| section_id(title).as_deref() == Some(identifier))
        })
    }
}

/// No directory symlinks: they can cycle or pull unrelated Markdown into the spec.
fn markdown_paths(folder: &Path) -> io::Result<Vec<PathBuf>> {
    match fs::symlink_metadata(folder) {
        Ok(metadata) if metadata.file_type().is_symlink() => return Ok(Vec::new()),
        Ok(_) => (),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    }
    let entries = match fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = entry.path();
        if kind.is_dir() {
            paths.extend(markdown_paths(&path)?);
        } else if is_markdown(&path) && fs::metadata(&path)?.is_file() {
            paths.push(path);
        }
    }
    Ok(paths)
}

pub fn is_markdown(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

/// ATX headings outside fenced examples, regardless of their level or source file.
pub fn headings(markdown: &str) -> impl Iterator<Item = (usize, &str)> {
    visible_lines(markdown).filter_map(|line| {
        let line = line.trim_start();
        let level = line.bytes().take_while(|byte| *byte == b'#').count();
        (level > 0 && level <= 6 && line.as_bytes().get(level) == Some(&b' '))
            .then(|| (level, line[level..].trim().trim_end_matches('#').trim_end()))
    })
}

pub fn visible_lines(markdown: &str) -> impl Iterator<Item = &str> {
    let mut fence = None;
    markdown.lines().filter(move |line| {
        let trimmed = line.trim_start();
        let marker = trimmed.as_bytes().first().copied().unwrap_or_default();
        let count = trimmed.bytes().take_while(|byte| *byte == marker).count();
        if matches!(marker, b'`' | b'~') && count >= 3 {
            match fence {
                None => fence = Some((marker, count)),
                Some((opening, width)) if opening == marker && count >= width => fence = None,
                _ => (),
            }
            return false;
        }
        fence.is_none()
    })
}

/// Heading IDs such as `F5`, `§3.1` (also written `3.1`) and `R8` stay independent of paths.
pub fn section_id(title: &str) -> Option<String> {
    let label = title.trim_start_matches(['*', '`', ' ', '(']);
    let end = label
        .find(|character: char| {
            !character.is_ascii_alphanumeric() && !matches!(character, '.' | '§')
        })
        .unwrap_or(label.len());
    let label = label[..end].trim_end_matches('.');
    let number = label
        .trim_start_matches('§')
        .trim_start_matches(|character: char| character.is_ascii_uppercase());
    if number.is_empty()
        || !number.split('.').all(|part| {
            !part.is_empty() && part.chars().all(|character| character.is_ascii_digit())
        })
    {
        return None;
    }
    Some(
        if label.starts_with('§') || label.starts_with(|character: char| character.is_ascii_digit())
        {
            format!("§{number}")
        } else {
            label.to_owned()
        },
    )
}
