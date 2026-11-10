//! Pure F6 decisions. I/O supplies overlap evidence and exact checked commit identities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckScope {
    None,
    BuildAndLint,
    Full,
}

/// A moved base always requires at least a rebuild; a moved author head needs full checks.
pub fn check_scope(same_head: bool, same_base: bool, overlap: bool) -> CheckScope {
    if !same_head {
        CheckScope::Full
    } else if same_base {
        CheckScope::None
    } else if overlap {
        CheckScope::Full
    } else {
        CheckScope::BuildAndLint
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

#[cfg(test)]
mod tests {
    // Omitting a rebase or checking an old base can lose another agent's changes.
    #[test]
    fn lean_landing_fixtures() -> Result<(), Box<dyn std::error::Error>> {
        for line in include_str!("../../../proofs/fixtures/targeted-rebase.tsv").lines() {
            let fields: Vec<_> = line.split('\t').collect();
            let first = fields[0].parse()?;
            let second = fields[1].parse()?;
            let third = fields[2].parse()?;
            assert_eq!(
                super::needs_rebase(first, second, third),
                fields[3].parse::<bool>()?
            );
            let scope = match super::check_scope(first, second, third) {
                super::CheckScope::None => 0,
                super::CheckScope::BuildAndLint => 1,
                super::CheckScope::Full => 2,
            };
            assert_eq!(scope, fields[4].parse::<u32>()?);
        }
        Ok(())
    }
}
