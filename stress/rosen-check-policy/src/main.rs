//! Risk (audit of Velocity 59be908, findings c and d): the checker read the pre-land
//! commands before it staged a change's merge result, and the receipt's check set again
//! when it submitted, each at whatever the base was at that moment; and PR duty tiered
//! changes by the git checkout's AGENTS.md. A policy landing in between let the checker run
//! the old commands on the newer base and certify them, and left a newly sensitive path
//! reviewed by reading.
//!
//! This lands a `rosen.toml` change between two checks of another change, with a stale
//! AGENTS.md beside the project, and requires that:
//! - a receipt from before the policy land no longer lands the change;
//! - the merge result staged after it comes with the new commands, check set and
//!   sensitive paths, read at exactly the staged base;
//! - the current-base policy (what tiers a review) names the new sensitive path although
//!   the AGENTS.md on disk does not;
//! - the receipt for the new base names the new check set and lands the change.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rosen_client::protocol::Listener;
use rosen_core::{Commit, FileMode, Signature, Tree, TreeEntry};
use rosen_server::landing::Ledger;
use rosen_server::{ServerConfig, ServerState};
use rosen_store::{LooseObjectStore, ObjectStore};
use velocity_rosen::checker::{self, CHECKER};
use velocity_rosen::duty::{self, LandOutcome};
use velocity_rosen::policy::policy_at;
use velocity_rosen::{ProjectServer, create_workspace};

const OLD_POLICY: &str =
    "sensitive = [\"rosen.toml\"]\n\n[checks.pre_land]\ncommands = [\"echo old\"]\n";
const NEW_POLICY: &str = "sensitive = [\"rosen.toml\", \"scripts/check-ui.mjs\"]\n\n[checks.pre_land]\nname = \"pre-land-v2\"\ncommands = [\"echo new\"]\n";
const STALE_AGENTS: &str = "# AGENTS.md\n\n## Sensitive paths\n- `rosen.toml`: the policy\n";

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let scratch = std::env::temp_dir().join(format!("rosen-check-policy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    // The git checkout a project was added from keeps its old AGENTS.md: nothing reads it now.
    std::fs::write(scratch.join("AGENTS.md"), STALE_AGENTS).unwrap();
    let server = ProjectServer::new(&scratch.join("app-data"), 1);
    start_server(&server);

    let edit = author(
        &server,
        "author-a",
        &scratch.join("a"),
        "src/lib.rs",
        "pub fn a() {}\n",
    )
    .await;
    let policy = author(
        &server,
        "author-c",
        &scratch.join("c"),
        "rosen.toml",
        NEW_POLICY,
    )
    .await;
    let folder = scratch.join("pre-land-checks");

    let before = checker::stage_merge_result(&server, &edit, &folder)
        .await
        .unwrap();
    assert_eq!(before.policy.commands, ["echo old"]);
    assert_eq!(before.policy.base, before.merge.base);
    assert!(!before.policy.is_sensitive("scripts/check-ui.mjs"));
    println!(
        "staged {} on base {} with {:?}",
        short(&edit),
        short(&before.merge.base),
        before.policy.commands
    );

    check_and_land(&server, &policy, &scratch.join("policy-checks")).await;
    println!("the policy change {} landed", short(&policy));

    // The checks of the old base are no receipt for the new one.
    let old = (before.merge.head.as_str(), before.merge.base.as_str());
    duty::submit_receipt(&server, CHECKER, &edit, old)
        .await
        .unwrap();
    let refused = duty::land(&server, CHECKER, &edit).await.unwrap();
    assert!(
        matches!(&refused, LandOutcome::Refused { code, .. } if code != "unapproved"),
        "{refused:?}"
    );
    println!("a receipt from before the policy land is refused: {refused:?}");

    let after = checker::stage_merge_result(&server, &edit, &folder)
        .await
        .unwrap();
    assert_ne!(after.merge.base, before.merge.base);
    assert_eq!(after.policy.base, after.merge.base);
    assert_eq!(after.policy.commands, ["echo new"]);
    assert_eq!(after.policy.check_set, "pre-land-v2");
    assert!(after.policy.is_sensitive("scripts/check-ui.mjs"));
    let current = policy_at(&server, None).await.unwrap();
    assert!(
        current.is_sensitive("scripts/check-ui.mjs"),
        "the review tier missed the new sensitive path"
    );
    println!(
        "staged again on base {} with {:?}, check set {}",
        short(&after.merge.base),
        after.policy.commands,
        after.policy.check_set
    );

    let new = (after.merge.head.as_str(), after.merge.base.as_str());
    duty::submit_receipt(&server, CHECKER, &edit, new)
        .await
        .unwrap();
    let landed = duty::land(&server, CHECKER, &edit).await.unwrap();
    assert!(matches!(landed, LandOutcome::Landed { .. }), "{landed:?}");
    println!("the receipt naming pre-land-v2 landed it: {landed:?}");
    let _ = std::fs::remove_dir_all(&scratch);
    println!("PASS");
}

/// A server for `server`'s endpoint and data folder, whose base holds the old policy.
fn start_server(server: &ProjectServer) {
    let state = ServerState::new(ServerConfig {
        data_dir: server.data_dir().to_path_buf(),
        endpoint: server.endpoint().clone(),
        warm_pool_dir: None,
    });
    let store = LooseObjectStore::open(state.config.objects_dir()).unwrap();
    let mut entries = Vec::new();
    for (name, text) in [("rosen.toml", OLD_POLICY), ("README.md", "pilot\n")] {
        let blob = store.put_blob(text.as_bytes()).unwrap();
        entries.push(TreeEntry::new(FileMode::Regular, name, blob).unwrap());
    }
    let lib = store.put_blob(b"pub fn base() {}\n").unwrap();
    let src = Tree::new(vec![
        TreeEntry::new(FileMode::Regular, "lib.rs", lib).unwrap(),
    ])
    .unwrap();
    let src = store.put_tree(&src).unwrap();
    entries.push(TreeEntry::new(FileMode::Directory, "src", src).unwrap());
    let who = Signature::new("stress", "stress@example.invalid", 0);
    let commit = Commit {
        tree: store.put_tree(&Tree::new(entries).unwrap()).unwrap(),
        parents: vec![],
        author: who.clone(),
        committer: who,
        extra_headers: vec![],
        message: b"base\n".to_vec(),
    };
    let base = store.put_commit(&commit).unwrap();
    Ledger::open(&state.config.metadata_path())
        .unwrap()
        .init_base("main", &base)
        .unwrap();
    let listener = Listener::bind(server.endpoint()).unwrap();
    tokio::spawn(rosen_server::listener::serve(listener, Arc::clone(&state)));
}

/// A change by `agent` that writes `text` to `file`, named and approved.
async fn author(
    server: &ProjectServer,
    agent: &str,
    root: &Path,
    file: &str,
    text: &str,
) -> String {
    let workspace = create_workspace(server, agent, root, None).await.unwrap();
    let path: PathBuf = root.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
    let session = rosen_client::Session::builder()
        .endpoint(server.endpoint().clone())
        .agent(rosen_client::AgentId::new(agent).unwrap())
        .start_daemon(false)
        .open()
        .await
        .unwrap();
    session
        .snapshot(root, Some(format!("{agent} edits {file}")))
        .await
        .unwrap();
    let head = duty::change_standing(server, &workspace.change)
        .await
        .unwrap()
        .head;
    duty::approve(server, "pr-duty-stress", &workspace.change, &head)
        .await
        .unwrap();
    workspace.change
}

/// The checker's own path: stage, check (the commands are echoes), receipt, land.
async fn check_and_land(server: &ProjectServer, change: &str, folder: &Path) {
    for _ in 0..5 {
        let staged = checker::stage_merge_result(server, change, folder)
            .await
            .unwrap();
        let pair = (staged.merge.head.as_str(), staged.merge.base.as_str());
        duty::submit_receipt(server, CHECKER, change, pair)
            .await
            .unwrap();
        if let LandOutcome::Landed { .. } = duty::land(server, CHECKER, change).await.unwrap() {
            return;
        }
    }
    panic!("{change} never landed");
}

fn short(id: &str) -> &str {
    &id[..id.len().min(10)]
}
