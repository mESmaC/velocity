$ErrorActionPreference = 'Stop'
# PR #134 review: code changes that the F7 tier classifier reads as "docs, comments or
# formatting" (trivial). SPEC.md F7 says a change is trivial only when Velocity can show it.
$root = Split-Path -Parent $PSScriptRoot
$fixture = Join-Path $env:TEMP ('velocity-pr134-tier-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path (Join-Path $fixture 'src') -Force | Out-Null
$core = (Join-Path $root 'crates/velocity-core').Replace('\', '/')
@"
[package]
name = "pr134-tier"
version = "0.0.0"
edition = "2024"
[workspace]
[dependencies]
velocity-core = { path = "$core" }
"@ | Set-Content -LiteralPath (Join-Path $fixture 'Cargo.toml')
@'
use velocity_core::review_diff::summarize_diff;
use velocity_core::review_tier::assess_tier;

// Moves the authorization check after the delete: a reordering, not whitespace.
const REORDER: &str = "diff --git a/src/delete.rs b/src/delete.rs
index 1111111..2222222 100644
--- a/src/delete.rs
+++ b/src/delete.rs
@@ -1,4 +1,4 @@
 fn remove(path: &Path) -> Result<()> {
-    check_owner(path)?;
     std::fs::remove_dir_all(path)?;
+    check_owner(path)?;
 }
";

// Comments out a live statement by adding only `/*` and `*/` lines around it.
const BLOCK_WRAP: &str = "diff --git a/src/delete.rs b/src/delete.rs
index 1111111..2222222 100644
--- a/src/delete.rs
+++ b/src/delete.rs
@@ -1,3 +1,5 @@
 fn remove(path: &Path) -> Result<()> {
+    /*
     check_owner(path)?;
+    */
     std::fs::remove_dir_all(path)?;
";

// Changes a factor on a rustfmt continuation line, which starts with the `*` operator.
const CONTINUATION: &str = "diff --git a/src/price.rs b/src/price.rs
index 1111111..2222222 100644
--- a/src/price.rs
+++ b/src/price.rs
@@ -1,3 +1,3 @@
 let total = base_price_for_the_selected_plan
-    * monthly_rate;
+    * yearly_rate;
";

fn main() {
    let mut wrong = 0;
    for (name, diff) in [("reordered statements", REORDER), ("code wrapped in /* */", BLOCK_WRAP), ("operator continuation line", CONTINUATION)] {
        let tier = assess_tier(&summarize_diff(diff), &[]).tier;
        println!("{name}: {}", tier.label());
        if tier.label() == "trivial" {
            wrong += 1;
        }
    }
    if wrong > 0 {
        eprintln!("F7: {wrong} code change(s) tiered trivial");
        std::process::exit(1);
    }
}
'@ | Set-Content -LiteralPath (Join-Path $fixture 'src/main.rs')
$env:CARGO_TARGET_DIR = Join-Path $root 'target'
& (Join-Path $root 'scripts/build-queue.ps1') cargo run --quiet --manifest-path (Join-Path $fixture 'Cargo.toml')
exit $LASTEXITCODE
