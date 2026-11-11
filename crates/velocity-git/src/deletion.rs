//! Pure deletion guard for SPEC.md F5/F12 and the F17 proof list.

/// I/O supplies verified ownership and whether any work has not reached a remote.
pub fn deletion_allowed(merged: bool, thread_deleted: bool, owned: bool, unpushed: bool) -> bool {
    (merged || thread_deleted) && owned && !unpushed
}

#[cfg(test)]
mod tests {
    use super::deletion_allowed;

    // Risk: cleanup loses unpushed work or removes another thread's checkout.
    #[test]
    fn deletion_matches_lean_fixtures() -> Result<(), Box<dyn std::error::Error>> {
        let fixtures = include_str!("../../../proofs/fixtures/worktree_deletion.tsv");
        let mut rows = 0;
        for line in fixtures.lines() {
            let fields = line
                .split('\t')
                .map(str::parse::<bool>)
                .collect::<Result<Vec<_>, _>>()?;
            assert_eq!(fields.len(), 5);
            assert_eq!(
                deletion_allowed(fields[0], fields[1], fields[2], fields[3]),
                fields[4],
                "{line}"
            );
            rows += 1;
        }
        assert_eq!(rows, 16);
        Ok(())
    }
}
