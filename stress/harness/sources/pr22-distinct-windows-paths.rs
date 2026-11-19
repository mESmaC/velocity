use std::{fs, path::{Path, PathBuf}, process::exit, sync::atomic::AtomicBool};
use velocity_core::{worktree_settings::{WorktreeDisplayStatus, WorktreePreferences},
    worktree_view::{WorktreeContext, WorktreeOwner, footer_summary, path_key}};
use velocity_git::inventory::{WorktreeInventory, measure_worktree};

fn report(name: &str, passed: bool, detail: impl std::fmt::Display) -> usize {
    println!("{} {name}: {detail}", if passed { "PASS" } else { "FAIL" });
    usize::from(!passed)
}

fn owner(identifier: u32, path: &Path) -> WorktreeOwner {
    WorktreeOwner { identifier, title: format!("thread-{identifier}"),
        path: path.to_string_lossy().into_owned(), status: WorktreeDisplayStatus::Working, fixing: false }
}

fn context(root: &Path, upper: &Path, lower: &Path) -> WorktreeContext {
    WorktreeContext { revision: 0, project_identifier: Some(1), project_name: "project".into(),
        repository_path: None, merged_owners: vec![], owners: vec![owner(1, upper), owner(2, lower)],
        uses_rosen: false, other_version_control_threads: 0,
        preferences: WorktreePreferences { root: root.to_string_lossy().into_owned(),
            project_override: None, delete_local_branch: true, share_build_cache: true,
            copy_files: String::new(), setup_command: String::new(), ready_count: 4 } }
}

fn check(root: &Path) -> usize {
    // A user can change the root between launches; old worktrees stay put (F5).
    let upper = root.join("deseret-\u{10400}").join("project").join("thread");
    let lower = root.join("deseret-\u{10428}").join("project").join("thread");
    fs::create_dir_all(&upper).expect("first isolated directory");
    fs::create_dir_all(&lower).expect("second isolated directory");
    fs::write(upper.join("payload"), [0_u8; 7]).expect("first payload");
    fs::write(lower.join("payload"), [0_u8; 3]).expect("second payload");
    let distinct = fs::canonicalize(&upper).expect("canonical upper") != fs::canonicalize(&lower).expect("canonical lower")
        && fs::read(upper.join("payload")).expect("read upper").len() == 7
        && fs::read(lower.join("payload")).expect("read lower").len() == 3;
    let mut failures = report("native-directories-are-distinct", distinct, "separate canonical paths and 7/3-byte payloads");
    if !distinct { return failures; }
    failures += report("distinct-paths-have-distinct-keys", path_key(&upper.to_string_lossy()) != path_key(&lower.to_string_lossy()), "U+10400 and U+10428");
    let context = context(root, &upper, &lower);
    let mut inventory = WorktreeInventory::default();
    for path in [&upper, &lower] {
        inventory.update_size(path.to_string_lossy().into_owned(), measure_worktree(path, &AtomicBool::new(false)));
    }
    let view = inventory.snapshot(&context);
    failures += report("both-worktrees-retain-inventory-rows", view.live_count == "2", format!("expected=2, actual={}", view.live_count));
    failures += report("both-worktrees-retain-total-bytes", view.total_size == "10 B", format!("expected=10 B, actual={}", view.total_size));
    let (count, _) = footer_summary(&context.owners, &context.preferences.root);
    failures += report("both-worktrees-retain-footer-count", count == 2, format!("expected=2, actual={count}"));
    failures
}

fn main() {
    assert!(cfg!(windows), "This reproduction requires Windows filesystem semantics");
    let root = PathBuf::from(std::env::args().nth(1).expect("isolated fixture root"));
    let failures = check(&root);
    println!("failures: {failures}");
    exit(i32::from(failures != 0));
}
