// Risk (#386, SPEC.md F5): test-only post-land success closes an issue without
// evidence that a build passed. Pinned Rosen 56ca3c3's post_land::deliver emits
// precisely {commit} for every successful post-land job, including a check set
// containing only tests (or no commands). It supplies no check_kind or kind.
use velocity_rosen::{IssueEvent, ProjectEvent, issue_event};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let event: ProjectEvent = serde_json::from_value(serde_json::json!({
        "seq": 2,
        "kind": "green",
        "change": "test-only-change",
        "detail": { "commit": "a".repeat(40) }
    }))?;
    let accepted = matches!(issue_event(&event), Some((_, IssueEvent::BuildPassed(_))));
    println!("Pinned R12 green after test-only checks: accepted_as_build_pass={accepted}");
    if accepted {
        return Err("test-only green must not qualify an issue for closure".into());
    }
    Ok(())
}
