//! What Velocity tells the spec agent (F3). The interview runs on whichever provider you
//! chose, so everything it must do is in the prompt rather than in provider config.

use velocity_store::InterviewRecord;

use crate::envelope::FENCE_LABEL;

/// The first prompt: the brief, the three phases, and the block that ends each reply.
pub fn opening_prompt(project_name: &str, brief: &str, spec_path: &str) -> String {
    let brief = if brief.trim().is_empty() {
        "(no brief given: find out what to build in your first questions)"
    } else {
        brief.trim()
    };
    format!(
        "You are the spec interview agent for the project \"{project_name}\" in Velocity. \
Your working directory is the project's main checkout.

What the user wants to build: {brief}

Do this, in order:
1. Scan: look through the folder or repository and work out what is already there \
(languages, structure, docs, tests, build). Change nothing while you scan.
2. Ask: interview the user one question at a time until nothing about what to build is \
ambiguous. Each question has a short reason why it matters and up to 4 options, exactly one \
marked recommended. The user picks an option or answers in their own words.
3. Ready: when nothing is ambiguous, write {spec_path} at the project root. It records every \
decision, the scope, what is explicitly out of scope, and what done means. Then say you are \
ready, with the proposed proof list.

Rules:
- Edit only {spec_path}. Do not write AGENTS.md, code, tests or Lean proofs.
- The proof list names the few features whose algorithm, if wrong, loses work or breaks \
coordination (locks, merges, deletion, scheduling). UI and plumbing never go on it. It may be \
empty.
- After each answer, propose the draft tasks it adds to the Task List: concrete pieces of \
build work, each a short imperative title.
{}",
        reply_format()
    )
}

/// The format reminder every prompt ends with.
pub fn reply_format() -> String {
    format!(
        "
End every reply with exactly one block in this format, with nothing after it:

```{FENCE_LABEL}
{{\"summary\": \"\", \"tasks\": [], \"question\": {{\"key\": \"Stack\", \"text\": \"What should \
this be written in?\", \"why\": \"Sets the toolchain, CI and how it ships.\", \"options\": \
[{{\"label\": \"Rust\", \"detail\": \"one static binary\", \"recommended\": true}}, \
{{\"label\": \"Go\", \"detail\": \"one static binary\"}}]}}}}
```

- \"summary\": in your first reply only, one or two sentences on what the scan found.
- \"tasks\": the draft tasks the latest answer adds, or [].
- \"question\": the next question. \"key\" is a one-to-three word name for the decision it \
settles.
- When you are ready instead, write {{\"ready\": {{\"proof_list\": [\"Feature: why it needs a \
proof\"]}}}} in place of \"question\", after the spec file is written."
    )
}

/// An answer to the question on the card, by option or in your own words.
pub fn answer_prompt(key: &str, value: &str) -> String {
    format!("Answer for {key}: {value}\n{}", reply_format())
}

/// Keep grilling (`G`).
pub fn grill_prompt(spec_path: &str) -> String {
    format!(
        "Keep grilling: ask more questions about anything still open or under-specified. \
Update {spec_path} with each new decision before you say you are ready again.\n{}",
        reply_format()
    )
}

/// The agent said it is ready, but the spec file does not exist yet.
pub fn write_spec_prompt(spec_path: &str) -> String {
    format!(
        "You said you are ready, but {spec_path} does not exist at the project root. Write it now with every decision, the scope, what is out of scope and what done means, then reply ready again.
{}",
        reply_format()
    )
}

/// A message that is not an answer: a question for the agent, or a correction.
pub fn message_prompt(text: &str) -> String {
    format!("{text}\n{}", reply_format())
}

/// After a restart the agent's session is gone, so the new one starts from what
/// `state.json` kept (§3.2 Resume after a restart).
pub fn resume_prompt(project_name: &str, spec_path: &str, record: &InterviewRecord) -> String {
    let mut prompt = opening_prompt(project_name, &record.brief, spec_path);
    prompt.push_str(
        "\n\nThis interview was interrupted and is resuming. Skip the scan unless you need it.",
    );
    if !record.decisions.is_empty() {
        prompt.push_str(" Decisions already made:\n");
        for decision in &record.decisions {
            prompt.push_str(&format!("- {}: {}\n", decision.key, decision.value));
        }
    }
    prompt
}
