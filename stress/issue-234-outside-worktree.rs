// Risk (#234): a build thread's claim grew to include scratch files the agent wrote
// outside its worktree, recorded as absolute paths. Two agents writing to a shared temp
// folder then post overlap lines that mean nothing, and claims fill with machine paths.
// Checks the real velocity-airc library: Windows spellings of the worktree, files
// outside it, and a channel log written before the fix that already holds absolute paths.
// Run: `cargo run -p stress -- issue-234-outside-worktree`.
use velocity_airc::Channel;
use velocity_airc::claims::worktree_file;

fn report(name: &str, passed: bool, detail: &str) -> usize {
    println!("{} {name}: {detail}", if passed { "PASS" } else { "FAIL" });
    usize::from(!passed)
}

fn spellings() -> usize {
    let worktree = r"D:\velocity-wt\issue-221";
    let cases: [(&str, &str, Option<&str>); 12] = [
        (worktree, r"D:\velocity-wt\issue-221\SPEC.md", Some("SPEC.md")),
        (worktree, r"d:\velocity-wt\issue-221\crates\a.rs", Some("crates/a.rs")),
        (worktree, "D:/velocity-wt/issue-221/crates/a.rs", Some("crates/a.rs")),
        (r"D:\velocity-wt\issue-221\", r"D:\velocity-wt\issue-221\ui\x.ts", Some("ui/x.ts")),
        (worktree, r"\\?\D:\velocity-wt\issue-221\src\b.rs", Some("src/b.rs")),
        (r"\\?\d:\velocity-wt\issue-221", r"D:\velocity-wt\issue-221\c.rs", Some("c.rs")),
        (worktree, r"crates\relative.rs", Some("crates/relative.rs")),
        (
            worktree,
            r"D:\Users\Cameron\AppData\Local\Temp\claude\scratchpad\run221\cdp.mjs",
            None,
        ),
        (worktree, r"D:\velocity-wt\issue-221-other\SPEC.md", None),
        (worktree, r"D:\velocity-wt\issue-221", None),
        (worktree, r"..\issue-212\SPEC.md", None),
        (worktree, r"E:\velocity-wt\issue-221\SPEC.md", None),
    ];
    let mut failures = 0;
    for (root, path, expected) in cases {
        let actual = worktree_file(root, path);
        failures += report(
            &format!("{path} in {root}"),
            actual.as_deref() == expected,
            &format!("{actual:?}"),
        );
    }
    failures
}

fn overlaps(channel: &Channel) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    Ok(channel
        .read(0)?
        .into_iter()
        .filter(|entry| entry.kind == "overlap")
        .map(|entry| entry.text)
        .collect())
}

fn absolute_claims(channel: &Channel) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    Ok(channel
        .claims()?
        .into_iter()
        .flat_map(|claim| claim.files)
        .filter(|file| file.contains(':') && !file.contains("::"))
        .collect())
}

fn jsonl_files(directory: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
    for item in std::fs::read_dir(directory).into_iter().flatten().flatten() {
        let path = item.path();
        if path.is_dir() {
            jsonl_files(&path, found);
        } else if path.extension().is_some_and(|value| value == "jsonl") {
            found.push(path);
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut failures = spellings();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!("velocity-issue-234-{stamp}"));
    let (old, new) = (root.join("old"), root.join("new"));

    // Live path: an absolute observed file is never claimed.
    let channel = Channel::open(&old, "project")?;
    channel.join("thread-1", r"D:\velocity-wt\issue-221")?;
    channel.join("thread-2", r"D:\velocity-wt\issue-212")?;
    channel.claim("thread-1", "ACP Install", "vel/issue-221", vec!["legacy-a.md".into()])?;
    channel.claim("thread-2", "PR poll", "vel/issue-212", vec!["legacy-b.md".into()])?;
    channel.observe_file("thread-1", r"D:\Temp\scratchpad\cdp.mjs")?;
    failures += report(
        "observing an absolute file claims nothing",
        absolute_claims(&channel)?.is_empty(),
        &format!("{:?}", absolute_claims(&channel)?),
    );

    // A log written before the fix: both claims hold the same absolute scratch file.
    let mut logs = Vec::new();
    jsonl_files(&old, &mut logs);
    let scratch = "D:/Users/Cameron/AppData/Local/Temp/scratchpad/run221/cdp.mjs";
    for log in logs {
        let text = std::fs::read_to_string(&log)?
            .replace("legacy-a.md", scratch)
            .replace("legacy-b.md", scratch);
        let target = new.join(log.strip_prefix(&old)?);
        std::fs::create_dir_all(target.parent().ok_or("log has no folder")?)?;
        std::fs::write(target, text)?;
    }
    let legacy = Channel::open(&new, "project")?;
    failures += report(
        "legacy absolute paths are not held",
        absolute_claims(&legacy)?.is_empty(),
        &format!("{:?}", absolute_claims(&legacy)?),
    );
    legacy.observe_file("thread-2", "src/poll.rs")?;
    legacy.claim("thread-1", "ACP Install", "vel/issue-221", vec!["SPEC.md".into()])?;
    failures += report(
        "legacy absolute paths post no overlap",
        overlaps(&legacy)?.is_empty(),
        &format!("{:?}", overlaps(&legacy)?),
    );
    legacy.observe_file("thread-2", "SPEC.md")?;
    let real = overlaps(&legacy)?;
    failures += report(
        "a shared repository file still overlaps",
        real.len() == 1 && real[0].contains("SPEC.md"),
        &format!("{real:?}"),
    );
    std::fs::remove_dir_all(root)?;
    std::process::exit(i32::from(failures > 0));
}
