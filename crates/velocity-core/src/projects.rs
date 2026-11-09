//! Pure project selection policies for SPEC.md F1 and §7 M2.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectSource {
    Local,
    GitHub,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectLabel {
    Local,
    GitHub,
    Speccing,
}

/// The actual project data and search text remain opaque to selection. In the
/// application these are a borrow of the persisted projects and a real String,
/// rather than parallel numeric stand-ins for their contents.
pub struct ProjectSelection<Projects, Text> {
    pub projects: Projects,
    pub active_project: Option<u32>,
    pub search: Text,
    pub menu_open: bool,
}

pub fn project_label(source: ProjectSource, spec_locked: bool) -> ProjectLabel {
    if !spec_locked {
        ProjectLabel::Speccing
    } else {
        match source {
            ProjectSource::Local => ProjectLabel::Local,
            ProjectSource::GitHub => ProjectLabel::GitHub,
        }
    }
}

/// IDs are projected from the authoritative project records by the store while
/// holding its lock. Stop at the first match; unknown IDs have no side effects.
pub fn find_project(identifiers: &[u32], requested: u32) -> Option<usize> {
    let mut index = 0;
    while index < identifiers.len() {
        if identifiers[index] == requested {
            return Some(index);
        }
        index += 1;
    }
    None
}

pub fn switch_project<Projects, Text>(
    mut selection: ProjectSelection<Projects, Text>,
    identifiers: &[u32],
    requested: u32,
    empty_search: Text,
) -> (ProjectSelection<Projects, Text>, Option<usize>) {
    let found = find_project(identifiers, requested);
    if found.is_some() {
        selection.active_project = Some(requested);
        selection.search = empty_search;
        selection.menu_open = false;
    }
    (selection, found)
}
