//! Compare whole Rosen work items using the same counts as the token ledger.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde::Serialize;

use super::{Entry, Role, project_folder};
use crate::Error;

#[derive(Clone, Debug, Default, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct Comparison {
    pub enabled: Cohort,
    pub disabled: Cohort,
    pub handed_off: u32,
    pub savings_percent: Option<f64>,
    pub result: TrialResult,
}

#[derive(Clone, Debug, Default, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct Cohort {
    pub eligible: u32,
    pub matched: u32,
    pub tokens: String,
    pub review_rounds: u32,
    pub reverts: u32,
}

#[derive(Clone, Debug, Default, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub enum TrialResult {
    #[default]
    Collecting,
    Met,
    Missed,
}

pub struct Item {
    pub project: u32,
    pub change: String,
    pub configuration: String,
    pub source: String,
    pub crossed_at: u64,
    pub enabled: bool,
    pub handed_off: bool,
    pub review_rounds: u32,
    pub reverts: u32,
}

pub fn compare(data: &Path, items: Vec<Item>) -> Result<Comparison, Error> {
    type Pairs = (Vec<Item>, Vec<Item>);
    let mut totals = BTreeMap::new();
    let mut matched = BTreeMap::<_, Pairs>::new();
    for item in items {
        if let std::collections::btree_map::Entry::Vacant(entry) = totals.entry(item.project) {
            entry.insert(project_totals(data, item.project)?);
        }
        if !totals[&item.project].contains_key(&item.change) {
            continue;
        }
        let group = matched
            .entry((
                item.project,
                item.configuration.clone(),
                item.source.clone(),
            ))
            .or_default();
        if item.enabled {
            group.0.push(item);
        } else {
            group.1.push(item);
        }
    }
    let mut comparison = Comparison::default();
    let (mut enabled_tokens, mut disabled_tokens) = (0u64, 0u64);
    for (mut enabled, mut disabled) in matched.into_values() {
        enabled.sort_by_key(|item| (item.crossed_at, item.change.clone()));
        disabled.sort_by_key(|item| (item.crossed_at, item.change.clone()));
        comparison.enabled.eligible += enabled.len() as u32;
        comparison.disabled.eligible += disabled.len() as u32;
        for (enabled, disabled) in enabled.into_iter().zip(disabled) {
            comparison.handed_off += u32::from(enabled.handed_off);
            comparison.enabled.observe(&enabled);
            comparison.disabled.observe(&disabled);
            enabled_tokens =
                enabled_tokens.saturating_add(totals[&enabled.project][&enabled.change]);
            disabled_tokens =
                disabled_tokens.saturating_add(totals[&disabled.project][&disabled.change]);
        }
    }
    comparison.finish(enabled_tokens, disabled_tokens);
    Ok(comparison)
}

impl Cohort {
    fn observe(&mut self, item: &Item) {
        self.matched += 1;
        self.review_rounds = self.review_rounds.saturating_add(item.review_rounds);
        self.reverts = self.reverts.saturating_add(item.reverts);
    }
}

impl Comparison {
    fn finish(&mut self, enabled: u64, disabled: u64) {
        self.enabled.tokens = crate::usage::count(enabled);
        self.disabled.tokens = crate::usage::count(disabled);
        if disabled == 0 {
            return;
        }
        self.savings_percent = Some((1.0 - enabled as f64 / disabled as f64) * 100.0);
        if self.handed_off >= 10 && self.disabled.matched >= 10 {
            self.result = if u128::from(enabled) * 10 <= u128::from(disabled) * 7
                && self.enabled.review_rounds <= self.disabled.review_rounds
                && self.enabled.reverts <= self.disabled.reverts
            {
                TrialResult::Met
            } else {
                TrialResult::Missed
            };
        }
    }
}

fn project_totals(data: &Path, project: u32) -> Result<HashMap<String, u64>, Error> {
    use std::fs::File;
    use std::io::{BufRead, BufReader};

    let mut totals = HashMap::<_, u64>::new();
    let files = match std::fs::read_dir(project_folder(data, project)) {
        Ok(files) => files,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(totals),
        Err(error) => return Err(error.into()),
    };
    for file in files {
        let path = file?.path();
        if path
            .extension()
            .is_none_or(|extension| extension != "jsonl")
        {
            continue;
        }
        let file = File::open(path)?;
        file.lock_shared()?;
        for line in BufReader::new(file).lines() {
            let Ok(entry) = serde_json::from_str::<Entry>(&line?) else {
                continue;
            };
            if entry.subject.role == Role::Implementation
                && let Some(change) = entry.change
            {
                let total = totals.entry(change).or_default();
                for call in entry.calls {
                    *total = total
                        .saturating_add(call.counts.input)
                        .saturating_add(call.counts.output);
                }
            }
        }
    }
    Ok(totals)
}
