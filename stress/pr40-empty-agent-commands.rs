// Risk: an empty ACP command advertisement must withdraw commands for that session.
// Run against the actual velocity-core library; no reducer or projection code is copied.
use velocity_core::layout::{LayoutRow, ThreadGroup};
use velocity_core::layout_state::LayoutState;
use velocity_core::layout_view::{LayoutAction, LayoutData, ThreadEntry, ProviderColor};
use velocity_core::navigation::NavigationView;
use velocity_core::thread_view::ThreadLifecycle;
use velocity_core::thread_view_model::{AgentCommandView, ThreadDetailEntry};

fn command(name: &str) -> AgentCommandView {
    AgentCommandView::new(name.into(), format!("Agent command {name}"))
}

fn thread(identifier: usize, commands: Vec<AgentCommandView>) -> ThreadEntry {
    let detail = ThreadDetailEntry {
        elapsed_label: "0s".into(),
        agent_commands: commands,
        model_name: "fixture".into(),
        effort_label: "High".into(),
        ..ThreadDetailEntry::new(
            identifier,
            format!("Thread {identifier}"),
            format!("vel/thread-{identifier}"),
            format!(r"X:\fixture with spaces\thread-{identifier}"),
            ThreadLifecycle::Working,
            ProviderColor::Codex,
        )
    };
    ThreadEntry {
        detail: Some(detail.clone()),
        ..ThreadEntry::new(
            LayoutRow { agent_count: 1, ..LayoutRow::new(identifier, ThreadGroup::Working) },
            detail.title,
            String::new(),
            NavigationView::Thread,
        )
    }
}

fn menu(state: &LayoutState) -> Vec<String> {
    state.snapshot().slash_items.into_iter().map(|item| item.command).collect()
}

fn report(name: &str, passed: bool, actual: &[String]) -> usize {
    println!("{} {name}: {}", if passed { "PASS" } else { "FAIL" }, actual.join(", "));
    usize::from(!passed)
}

fn main() {
    let mut state = LayoutState::default();
    state.replace_data(LayoutData {
        // This is the new public fallback field added by the PR.
        agent_commands: vec![command("foreign_session")],
        threads: vec![thread(1, vec![command("local_session")]), thread(2, Vec::new())],
        ..LayoutData::default()
    });
    state.apply(LayoutAction::ComposerText("/".into()));
    let home = menu(&state);
    let mut failures = report("home-fallback-control", home.iter().any(|item| item == "/foreign_session"), &home);
    state.apply(LayoutAction::OpenThread(1));
    state.apply(LayoutAction::ComposerText("/".into()));
    let advertised = menu(&state);
    failures += report("nonempty-advertisement-isolated",
        advertised.iter().any(|item| item == "/local_session")
            && !advertised.iter().any(|item| item == "/foreign_session"), &advertised);

    // ACP available_commands_update may replace the list with an empty list.
    state.set_thread_agent_commands(1, Vec::new());
    let withdrawn = menu(&state);
    failures += report("empty-update-withdraws-agent-commands", withdrawn.len() == 5, &withdrawn);
    state.apply(LayoutAction::ComposerText("/foreign".into()));
    state.apply(LayoutAction::SlashComplete);
    println!("After withdrawal, completing /foreign produced {:?}", state.snapshot().composer_text);
    println!("Submitting that completion emitted {:?}", state.apply(LayoutAction::SubmitComposer));

    state.apply(LayoutAction::OpenThread(2));
    state.apply(LayoutAction::ComposerText("/".into()));
    let fresh = menu(&state);
    failures += report("thread-with-no-advertisement-isolated", fresh.len() == 5, &fresh);
    state.set_thread_agent_commands(2, vec![command("second_session")]);
    let second = menu(&state);
    failures += report("second-session-update-control",
        second.iter().any(|item| item == "/second_session")
            && !second.iter().any(|item| item == "/foreign_session"), &second);
    println!("failures: {failures}");
    std::process::exit(i32::from(failures != 0));
}
