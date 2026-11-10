//! The axiom audit (SPEC.md F17): every declaration in the changed proof modules may depend
//! only on `propext`, `Classical.choice` and `Quot.sound`.
//!
//! A `#print axioms` line in the file covers only the theorems its author chose to print, so
//! Velocity writes its own audit file instead. It imports the changed modules and asks Lean for
//! the axioms of every declaration they define. `native_decide` shows up as `Lean.ofReduceBool`,
//! `sorry` as `sorryAx`, and an `axiom` declaration as itself.

/// The only axioms a proof may depend on.
pub const ALLOWED_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];

const AXIOM_LINE: &str = "velocity-axiom";
const AUDITED_LINE: &str = "velocity-audited";

/// A declaration that depends on an axiom outside [`ALLOWED_AXIOMS`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisallowedAxiom {
    pub declaration: String,
    pub axiom: String,
}

/// What the audit found: how many declarations it read, and the disallowed dependencies.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AxiomAudit {
    pub audited: usize,
    pub disallowed: Vec<DisallowedAxiom>,
}

/// The Lake module a proof file defines: `proofs/ClaimLock/Design.lean` is `ClaimLock.Design`
/// when `proofs` is the Lake project folder.
pub fn module_name(project_folder: &str, path: &str) -> Option<String> {
    let path = path.replace('\\', "/");
    let folder = project_folder.replace('\\', "/");
    let relative = if folder.is_empty() {
        path.as_str()
    } else {
        path.strip_prefix(&folder)?.strip_prefix('/')?
    };
    let module = relative.strip_suffix(".lean")?;
    (!module.is_empty() && !module.starts_with(".lake/")).then(|| module.replace('/', "."))
}

/// The audit file for `modules`. Its output lines are read by [`parse_audit`].
pub fn audit_script(modules: &[String]) -> String {
    let imports: String = modules
        .iter()
        .map(|module| format!("import {}\n", lean_name(module)))
        .collect();
    let names: Vec<String> = modules
        .iter()
        .map(|module| format!("`{}", lean_name(module)))
        .collect();
    let allowed: Vec<String> = ALLOWED_AXIOMS
        .iter()
        .map(|axiom| format!("``{axiom}"))
        .collect();
    format!(
        "import Lean\n{imports}open Lean Elab Command\n\n\
         #eval show CommandElabM Unit from do\n\
         \x20 let env ← getEnv\n\
         \x20 let allowed : List Name := [{allowed}]\n\
         \x20 let mut audited := 0\n\
         \x20 for module in ([{names}] : List Name) do\n\
         \x20   let some index := env.getModuleIdx? module\n\
         \x20     | throwError \"{AUDITED_LINE}: module {{module}} was not imported\"\n\
         \x20   for name in env.header.moduleData[index.toNat]!.constNames do\n\
         \x20     if name.isInternal then continue\n\
         \x20     audited := audited + 1\n\
         \x20     let axioms ← liftCoreM <| collectAxioms name\n\
         \x20     for axiomName in axioms do\n\
         \x20       unless allowed.contains axiomName do\n\
         \x20         IO.println s!\"{AXIOM_LINE}\\t{{name}}\\t{{axiomName}}\"\n\
         \x20 IO.println s!\"{AUDITED_LINE}\\t{{audited}}\"\n",
        allowed = allowed.join(", "),
        names = names.join(", "),
    )
}

/// Reads the audit's output. `None` when the audit never finished, so a crash or an import
/// error never reads as a clean audit.
pub fn parse_audit(output: &str) -> Option<AxiomAudit> {
    let mut audit = AxiomAudit::default();
    let mut finished = false;
    for line in output.lines() {
        let fields: Vec<&str> = line.trim_end_matches('\r').split('\t').collect();
        match fields.as_slice() {
            [AXIOM_LINE, declaration, axiom] => audit.disallowed.push(DisallowedAxiom {
                declaration: (*declaration).to_owned(),
                axiom: (*axiom).to_owned(),
            }),
            [AUDITED_LINE, count] => {
                audit.audited = count.trim().parse().ok()?;
                finished = true;
            }
            _ => {}
        }
    }
    finished.then_some(audit)
}

/// Escapes each name component Lean couldn't read bare (spaces, leading digits) with «».
fn lean_name(module: &str) -> String {
    module
        .split('.')
        .map(|part| {
            let bare = part
                .chars()
                .next()
                .is_some_and(|first| first.is_alphabetic() || first == '_')
                && part.chars().all(|character| {
                    character.is_alphanumeric() || matches!(character, '_' | '\'')
                });
            if bare {
                part.to_owned()
            } else {
                format!("«{part}»")
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}
