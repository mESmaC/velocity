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

#[cfg(test)]
mod tests {
    use super::{PostLandCheckPlan, PreLandCheckPlan};

    fn parse_pre_land_action(label: &str) -> Result<PreLandCheckPlan, String> {
        match label {
            "refuse_preflight" => Ok(PreLandCheckPlan::RefusePreflight),
            "check_bindings" => Ok(PreLandCheckPlan::RunBindingsCheck),
            "land" => Ok(PreLandCheckPlan::LandNow),
            other => Err(format!("unknown pre_land action: {other}")),
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

    // Risk (#629): omitting a rebase or skipping a required Rosen pre-land/post-land check
    // risks landing stale snapshots, stale bindings, or unverified commits.
    // Missing an eligible land request can also strand named work.
    #[test]
    fn lean_landing_fixtures() -> Result<(), Box<dyn std::error::Error>> {
        let (mut rebase_rows, mut pre_land_rows, mut post_land_rows, mut request_rows) =
            (0, 0, 0, 0);
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
                other => {
                    return Err(format!("unexpected targeted-rebase fixture row: {other:?}").into());
                }
            }
        }
        assert_eq!(
            (rebase_rows, pre_land_rows, post_land_rows, request_rows),
            (8, 8, 8, 16)
        );
        Ok(())
    }
}
