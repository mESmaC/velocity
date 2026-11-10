//! F11 Covers: the topics every AGENTS.md needs, and Add, which inserts editable bullets for a
//! missing one into the right section. The bullets are the default template's own wording.

use crate::agents_editor::{AgentsEditor, OutlineFocus};
use crate::agents_markdown::{OutlineKind, OutlineLine};

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct CoverageItem {
    /// Position in the Covers list; `OutlineAction::AddCoverage` names an item by it.
    pub item: u32,
    pub title: String,
    pub covered: bool,
}

/// How a topic counts as covered.
enum Detect {
    /// A `##` section whose title starts with this, ignoring case.
    Section(&'static str),
    /// Any of these phrases anywhere in the file, ignoring case.
    Phrase(&'static [&'static str]),
    /// This exact word anywhere in the file.
    Word(&'static str),
}

struct Topic {
    title: &'static str,
    detect: Detect,
    section: &'static str,
    /// Markdown bullets, nested ones indented by two spaces.
    rules: &'static [&'static str],
}

const TOPICS: &[Topic] = &[
    Topic {
        title: "Build, proof and lint commands",
        detect: Detect::Section("commands"),
        section: "Commands",
        rules: &[
            "- Build: `…`",
            "- Proofs: `lake build` in `proofs/`. Run it before every push.",
            "- Lint: `…`. Zero warnings.",
        ],
    },
    Topic {
        title: "Implementation first",
        detect: Detect::Section("implementation first"),
        section: "Implementation first",
        rules: &[
            "- Build the feature first, end to end, then run it and see it work.",
            "- Implementation is the larger part of every PR. Proofs, tests and docs together stay smaller than the implementation code.",
        ],
    },
    Topic {
        title: "Proofs: proof list only, after the code",
        detect: Detect::Section("proofs"),
        section: "Proofs",
        rules: &[
            "- Write Lean only for features on the proof list in SPEC.md. Every other piece ships with no Lean.",
            "- For a proof-list feature, write the code first. Then write `proofs/<Feature>/Design.lean` in the same PR: correct, within a stated cost bound, and no alternative in a stated class costs less.",
            "- Export a compact fixture table to `proofs/fixtures/<feature>.tsv`, and add one test that checks the implementation against it.",
            "- Never use `sorry`, `admit`, `axiom` or `native_decide`.",
            "- Never weaken a reviewed theorem statement to make its proof go through.",
        ],
    },
    Topic {
        title: "Tests and docs: low priority",
        detect: Detect::Section("tests"),
        section: "Tests and docs",
        rules: &[
            "- Default: add no tests. Add a targeted test only for a critical contract, with its risk in a comment.",
            "- Scouts and PR duty agents write the adversarial tests and stress scripts.",
        ],
    },
    Topic {
        title: "Scope: stay on your task",
        detect: Detect::Section("scope"),
        section: "Scope",
        rules: &[
            "- Work only on the piece you claimed.",
            "- If you find an unrelated problem, file an issue for it. Do not fix it in your PR.",
        ],
    },
    Topic {
        title: "PR: monitor until merged",
        detect: Detect::Phrase(&[
            "until merged",
            "until it is merged",
            "until it's merged",
            "until it\u{2019}s merged",
        ]),
        section: "Pull requests",
        rules: &[
            "- Opening the PR does not finish the task. Monitor it until it is merged.",
            "  - Fix failing checks on your change and push.",
        ],
    },
    Topic {
        title: "PR: address every review comment",
        detect: Detect::Phrase(&[
            "address every comment",
            "address every review comment",
            "address every pr duty comment",
        ]),
        section: "Pull requests",
        rules: &[
            "- Address every PR duty comment: fix and push, or reply with the reason. Resolve each thread.",
            "- After pushing fixes, re-request review.",
        ],
    },
    Topic {
        title: "Definition of done",
        detect: Detect::Section("done"),
        section: "Done means",
        rules: &[
            "- The piece meets SPEC.md and has been run successfully.",
            "- The required local checks pass on the merge result and the required proofs check.",
            "- The PR is merged.",
        ],
    },
    Topic {
        title: "AIRC: claims, overlaps, dependencies",
        detect: Detect::Word("AIRC"),
        section: "Coordination (AIRC)",
        rules: &[
            "- Before editing, claim your task, branch and every file you expect to touch in AIRC.",
            "- If your claim is refused, pick different unclaimed work.",
            "- Before editing a file another agent claimed, record a share or a wait in AIRC.",
            "- If your work depends on another PR, record a wait; continue and rebase when it merges.",
            "- Message another agent only when a claim, overlap or dependency needs words. Never repeat events Velocity posts.",
        ],
    },
    Topic {
        title: "Hard \u{201c}never\u{201d} list",
        detect: Detect::Section("never"),
        section: "Never",
        rules: &[
            "- Never push directly to the project's base branch.",
            "- Never merge your own PR. PR duty agents merge.",
            "- Never skip, delete or weaken a failing test to make checks pass.",
            "- Never commit secrets, tokens or `.env` files.",
        ],
    },
];

fn covered(topic: &Topic, lines: &[OutlineLine], markdown: &str) -> bool {
    match topic.detect {
        Detect::Section(prefix) => lines.iter().any(|line| {
            line.kind == OutlineKind::Section
                && line.text.trim_start().to_lowercase().starts_with(prefix)
        }),
        Detect::Phrase(phrases) => {
            let lowered = markdown.to_lowercase();
            phrases.iter().any(|phrase| lowered.contains(phrase))
        }
        Detect::Word(word) => markdown.contains(word),
    }
}

impl AgentsEditor {
    pub(crate) fn coverage(&self, markdown: &str) -> Vec<CoverageItem> {
        TOPICS
            .iter()
            .enumerate()
            .map(|(index, topic)| CoverageItem {
                item: u32::try_from(index).unwrap_or(u32::MAX),
                title: topic.title.into(),
                covered: covered(topic, &self.lines, markdown),
            })
            .collect()
    }

    /// Adds a topic's bullets after the last rule of its section, creating the section at the
    /// end of the file when none has that name. Focus lands on the first new bullet.
    pub(crate) fn add_coverage(&mut self, item: u32) -> Option<OutlineFocus> {
        let topic = TOPICS.get(usize::try_from(item).ok()?)?;
        let at = match self.section_end(topic.section) {
            Some(at) => at,
            None => {
                let end = self.lines.len();
                // One blank line separates the new section from the one before it.
                let separate = self
                    .lines
                    .last()
                    .is_some_and(|line| line.kind != OutlineKind::Source || !line.text.is_empty());
                let at = if separate { end + 1 } else { end };
                if separate {
                    self.insert(end, "");
                }
                self.insert(at, &format!("## {}", topic.section));
                at + 1
            }
        };
        let mut first = None;
        for (offset, rule) in topic.rules.iter().enumerate() {
            let identifier = self.insert(at + offset, rule);
            first.get_or_insert(identifier);
        }
        self.revision += 1;
        first.map(|identifier| OutlineFocus {
            identifier,
            at_end: true,
        })
    }

    /// The index just after the last rule (or the heading) of the named section.
    fn section_end(&self, title: &str) -> Option<usize> {
        let start = self.lines.iter().position(|line| {
            line.kind == OutlineKind::Section && line.text.trim().eq_ignore_ascii_case(title)
        })?;
        let end = self.lines[start + 1..]
            .iter()
            .position(|line| line.kind == OutlineKind::Section)
            .map_or(self.lines.len(), |offset| start + 1 + offset);
        let last = self.lines[start + 1..end]
            .iter()
            .rposition(|line| line.kind == OutlineKind::Rule)
            .map_or(start, |offset| start + 1 + offset);
        Some(last + 1)
    }
}
