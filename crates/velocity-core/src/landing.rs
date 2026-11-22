//! Pure F6/F7 landing and rebase decisions. Native I/O supplies overlap evidence,
//! preflight attestations, bindings status and Rosen's post-land policy.

/// Pre-land check plan for a named Rosen change (SPEC.md F6/F7, #503, #560).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreLandCheckPlan {
    /// Snapshot lacks a passing preflight; refuse before staging any merge check.
    RefusePreflight,
    /// Preflight passed; stage the merge and run `export-bindings --check` before landing.
    RunBindingsCheck,
    /// Preflight passed and bindings are either untouched or already verified; land now.
    LandNow,
}

impl PreLandCheckPlan {
    /// Staged pre-land merge check passes Velocity runs for this decision (0 or 1).
    pub const fn staged_passes(self) -> u32 {
        match self {
            Self::RunBindingsCheck => 1,
            Self::RefusePreflight | Self::LandNow => 0,
        }
    }
}

/// Decides what pre-land check work Velocity performs before asking Rosen to land
/// a named change (SPEC.md F6/F7, #503, #560).
pub fn pre_land_check_plan(
    preflight_current: bool,
    touches_bindings: bool,
    bindings_verified: bool,
) -> PreLandCheckPlan {
    if !preflight_current {
        PreLandCheckPlan::RefusePreflight
    } else if touches_bindings && !bindings_verified {
        PreLandCheckPlan::RunBindingsCheck
    } else {
        PreLandCheckPlan::LandNow
    }
}

/// Post-land verification pass plan for a landed Rosen commit (SPEC.md F7, #567).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PostLandCheckPlan {
    /// Whether the dedicated build worker runs a separate `CheckKind::Build` pass.
    pub separate_build_pass: bool,
    /// Whether the integration/test pass prepends `pre_land` build/lint commands.
    pub prepend_pre_land_to_test: bool,
}

impl PostLandCheckPlan {
    /// Total post-land verification passes executed for the batch (1 or 2).
    pub const fn passes(self) -> u32 {
        1 + self.separate_build_pass as u32
    }
}

/// Decides how Velocity schedules Rosen's post-land build and test verification passes
/// for a landed commit (SPEC.md F7, #567).
pub fn post_land_check_plan(
    after_land: bool,
    build_prefix_required: bool,
    has_full_suite: bool,
) -> PostLandCheckPlan {
    PostLandCheckPlan {
        separate_build_pass: !after_land && build_prefix_required,
        prepend_pre_land_to_test: after_land && !has_full_suite,
    }
}

/// Generated artifacts and explicitly scoped registration claims never trigger rebases.
pub fn contested_file(file: &str) -> bool {
    !matches!(
        file,
        "Cargo.lock" | "ui/pnpm-lock.yaml" | "ui/src/lib/bindings.ts"
    ) && !file.ends_with("::registration")
}

/// Claimed files are conservative; measured edits use touching regions with three lines of context.
pub fn region_overlap(
    left_start: u32,
    left_length: u32,
    right_start: u32,
    right_length: u32,
) -> bool {
    left_start <= right_start.saturating_add(right_length).saturating_add(3)
        && right_start <= left_start.saturating_add(left_length).saturating_add(3)
}

pub fn needs_rebase(claimed: bool, edited_overlap: bool, contested: bool) -> bool {
    contested && (claimed || edited_overlap)
}

/// Selects a named change under after-land policy or for a switch to that policy.
pub fn landing_request(
    after_land: bool,
    policy_switch: bool,
    named: bool,
    change: Option<&str>,
) -> Option<&str> {
    if (after_land || policy_switch) && named {
        change
    } else {
        None
    }
}

/// Category of a Rosen land refusal for the F6 retry and handoff policy (#487, #633).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LandRefusalKind {
    /// Rosen requires generated files to be regenerated on the merged inputs.
    Regenerate,
    /// Rosen reported merge conflicts to resolve in the workspace.
    Conflict,
    /// Rosen's before-land policy rejected the land (`unapproved`, `no_receipt`, `receipt_stale`).
    BeforeLandPolicy,
    /// Any other refusal without a built-in repair path; eligible for one retry per generation.
    Ordinary,
}

impl LandRefusalKind {
    /// Classifies a `LandRefusal::Other` code into before-land policy vs ordinary refusal.
    pub fn from_other_code(code: &str) -> Self {
        match code {
            "unapproved" | "no_receipt" | "receipt_stale" => Self::BeforeLandPolicy,
            _ => Self::Ordinary,
        }
    }

    /// Whether this refusal hands back to the author immediately without retrying.
    pub const fn immediate_handoff(self) -> bool {
        match self {
            Self::Regenerate | Self::Conflict | Self::BeforeLandPolicy => true,
            Self::Ordinary => false,
        }
    }
}

/// Decision for one Rosen land refusal on a named change generation (SPEC.md F6, #487, #633).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefusalAction {
    /// The refusal is for an older generation or settled thread; ignore so a newer head stays open.
    Ignore,
    /// A project prerequisite check is running; retry later without spending the retry slot.
    DeferPrerequisite,
    /// First ordinary refusal for this named generation; record it and retry once on a later pass.
    RetryOnce,
    /// Return the change to Working and deliver repair instructions to its author.
    Handoff,
}

impl RefusalAction {
    /// Retry attempts scheduled by this decision (0 or 1).
    pub const fn retry_attempts(self) -> u32 {
        match self {
            Self::DeferPrerequisite | Self::RetryOnce => 1,
            Self::Ignore | Self::Handoff => 0,
        }
    }

    /// Author handoff submissions triggered by this decision (0 or 1).
    pub const fn handoff_submissions(self) -> u32 {
        match self {
            Self::Handoff => 1,
            Self::Ignore | Self::DeferPrerequisite | Self::RetryOnce => 0,
        }
    }
}

/// Decides whether to ignore a stale refusal, defer while a prerequisite check runs,
/// retry an ordinary refusal once for the named generation, or hand back to the author
/// (SPEC.md F6, #487, #633).
pub fn refusal_plan(
    same_generation: bool,
    kind: LandRefusalKind,
    prerequisite_check_running: bool,
    already_retried: bool,
) -> RefusalAction {
    if !same_generation {
        RefusalAction::Ignore
    } else if kind.immediate_handoff() {
        RefusalAction::Handoff
    } else if prerequisite_check_running {
        RefusalAction::DeferPrerequisite
    } else if !already_retried {
        RefusalAction::RetryOnce
    } else {
        RefusalAction::Handoff
    }
}

#[cfg(test)]
mod tests {
    use super::{LandRefusalKind, PostLandCheckPlan, PreLandCheckPlan, RefusalAction};

    fn parse_pre_land_action(label: &str) -> Result<PreLandCheckPlan, String> {
        match label {
            "refuse_preflight" => Ok(PreLandCheckPlan::RefusePreflight),
            "check_bindings" => Ok(PreLandCheckPlan::RunBindingsCheck),
            "land" => Ok(PreLandCheckPlan::LandNow),
            other => Err(format!("unknown pre_land action: {other}")),
        }
    }

    fn parse_refusal_kind(label: &str) -> Result<LandRefusalKind, String> {
        match label {
            "regenerate" => Ok(LandRefusalKind::Regenerate),
            "conflict" => Ok(LandRefusalKind::Conflict),
            "before_land_policy" => Ok(LandRefusalKind::BeforeLandPolicy),
            "ordinary" => Ok(LandRefusalKind::Ordinary),
            other => Err(format!("unknown refusal kind: {other}")),
        }
    }

    fn parse_refusal_action(label: &str) -> Result<RefusalAction, String> {
        match label {
            "ignore" => Ok(RefusalAction::Ignore),
            "defer_prerequisite" => Ok(RefusalAction::DeferPrerequisite),
            "retry_once" => Ok(RefusalAction::RetryOnce),
            "handoff" => Ok(RefusalAction::Handoff),
            other => Err(format!("unknown refusal action: {other}")),
        }
    }

    fn check_landing_request(fields: &[&str]) -> Result<(), std::str::ParseBoolError> {
        let after_land = fields[1].parse()?;
        let named = fields[2].parse()?;
        let candidate = fields[3].parse::<bool>()?.then_some("named-change");
        let policy_switch = fields[4].parse()?;
        let expected = fields[5].parse::<bool>()?.then_some("named-change");
        assert_eq!(
            super::landing_request(after_land, policy_switch, named, candidate),
            expected,
        );
        Ok(())
    }

    fn check_refusal(fields: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
        let action = super::refusal_plan(
            fields[1].parse()?,
            parse_refusal_kind(fields[2])?,
            fields[3].parse()?,
            fields[4].parse()?,
        );
        assert_eq!(action, parse_refusal_action(fields[5])?);
        assert_eq!(action.retry_attempts(), fields[6].parse::<u32>()?);
        assert_eq!(action.handoff_submissions(), fields[7].parse::<u32>()?);
        Ok(())
    }

    // Risk (#629, #633): omitting a rebase, skipping a required Rosen pre-land/post-land check,
    // missing an eligible land request, or mishandling a refusal retry/handoff risks landing
    // stale snapshots, stranding named work, or reopening a newer generation.
    #[test]
    fn lean_landing_fixtures() -> Result<(), Box<dyn std::error::Error>> {
        let (
            mut rebase_rows,
            mut pre_land_rows,
            mut post_land_rows,
            mut request_rows,
            mut refusal_rows,
        ) = (0, 0, 0, 0, 0);
        for line in include_str!("../../../proofs/fixtures/targeted-rebase.tsv").lines() {
            let fields: Vec<_> = line.split('\t').collect();
            match fields.first().copied() {
                Some("rebase") => {
                    rebase_rows += 1;
                    assert_eq!(
                        super::needs_rebase(
                            fields[1].parse()?,
                            fields[2].parse()?,
                            fields[3].parse()?,
                        ),
                        fields[4].parse::<bool>()?,
                    );
                }
                Some("pre_land") => {
                    pre_land_rows += 1;
                    let plan = super::pre_land_check_plan(
                        fields[1].parse()?,
                        fields[2].parse()?,
                        fields[3].parse()?,
                    );
                    assert_eq!(plan, parse_pre_land_action(fields[4])?);
                    assert_eq!(plan.staged_passes(), fields[5].parse::<u32>()?);
                }
                Some("post_land") => {
                    post_land_rows += 1;
                    let plan = super::post_land_check_plan(
                        fields[1].parse()?,
                        fields[2].parse()?,
                        fields[3].parse()?,
                    );
                    assert_eq!(
                        plan,
                        PostLandCheckPlan {
                            separate_build_pass: fields[4].parse()?,
                            prepend_pre_land_to_test: fields[5].parse()?,
                        },
                    );
                    assert_eq!(plan.passes(), fields[6].parse::<u32>()?);
                }
                Some("landing_request") => {
                    request_rows += 1;
                    check_landing_request(&fields)?;
                }
                Some("refusal") => {
                    refusal_rows += 1;
                    check_refusal(&fields)?;
                }
                other => {
                    return Err(format!("unexpected targeted-rebase fixture row: {other:?}").into());
                }
            }
        }
        assert_eq!(
            (
                rebase_rows,
                pre_land_rows,
                post_land_rows,
                request_rows,
                refusal_rows,
            ),
            (8, 8, 8, 16, 32)
        );
        Ok(())
    }
}
