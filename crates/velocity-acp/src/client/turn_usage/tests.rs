use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

use agent_client_protocol::v1::schema::PromptResponse;
use velocity_graph::usage::{Totals, read};

use super::*;

#[tokio::test]
async fn recorded_prompt_usage_matches_provider_logs() -> Result<(), Box<dyn std::error::Error>> {
    // A larger second prompt must still count in full for these adapters (#466).
    let data = Scratch::new()?;
    for (provider, graph_on, fixture) in [
        (
            Provider::Anthropic,
            true,
            include_str!("fixtures/claude-0.106.0.json"),
        ),
        (
            Provider::Codex,
            false,
            include_str!("fixtures/codex-2.6.0.json"),
        ),
    ] {
        let mut fixture: serde_json::Value = serde_json::from_str(fixture)?;
        let responses: Vec<PromptResponse> = serde_json::from_value(fixture["responses"].take())?;
        let log: Vec<BTreeMap<String, u64>> = serde_json::from_value(fixture["log"].take())?;
        let mut prompt_usage = PromptUsage::for_provider(Some(provider));
        let meter = data.meter(graph_on);
        for response in &responses {
            meter
                .record(
                    prompt_usage.turn(response.usage.as_ref()),
                    Duration::from_secs(1),
                    SourceReads::default(),
                    |error| panic!("{error}"),
                )
                .await;
        }

        let saved = read(&data.0, "1");
        let side = if graph_on { saved.on } else { saved.off };
        let sum = |field: &str| log.iter().filter_map(|entry| entry.get(field)).sum::<u64>();
        let expected = if graph_on {
            sum("input_tokens")
                + sum("output_tokens")
                + sum("cache_read_input_tokens")
                + sum("cache_creation_input_tokens")
        } else {
            sum("total_tokens")
        };
        assert!(expected > 0);
        assert_eq!(side.turns_with_tokens, responses.len() as u64);
        assert_eq!(side.tokens.total, expected, "{provider:?}");
        assert_eq!(side.tokens.thought_tokens, sum("reasoning_output_tokens"));
    }
    Ok(())
}

#[test]
fn session_total_provider_counts_the_difference() {
    let mut meter = PromptUsage::for_provider(Some(Provider::Custom(1)));
    let first = Usage::new(12, 10, 2).thought_tokens(1);
    let next = Usage::new(29, 24, 5).thought_tokens(3);
    assert_eq!(meter.turn(Some(&first)), Some(counts(&first)));
    assert_eq!(meter.turn(None), None);
    assert_eq!(
        meter.turn(Some(&next)),
        Some(TokenCounts {
            input: 14,
            output: 3,
            thought_tokens: 2,
            total: 17,
            ..TokenCounts::default()
        })
    );
}

#[tokio::test]
async fn first_new_turn_retries_write_and_persists_reset() -> Result<(), Box<dyn std::error::Error>>
{
    let data = Scratch::new()?;
    let folder = data.0.join("graph").join("1");
    fs::create_dir_all(&folder)?;
    let legacy = Totals {
        turns: 6,
        ..Totals::default()
    };
    fs::write(
        folder.join("usage.json"),
        serde_json::to_vec(&serde_json::json!({
            "enabled": false, "on": legacy, "off": legacy, "since": "2026-11-17T00:00:00Z",
            "tools": { "graph_find": { "calls": 1, "exact": 1, "classified": 0 } },
            "workspaces": { "old": { "name": "old", "totals": legacy, "raw_reads": 2 } }
        }))?,
    )?;
    let migrated = read(&data.0, "1");
    assert!(!migrated.enabled);
    assert_eq!(migrated.off.turns, 0);
    assert_eq!(migrated.on.turns, 0);
    assert!(migrated.since.is_empty());

    // Fail the first write before the atomic rename, then let the same turn retry.
    let blocked_path = folder.join("usage.json.tmp");
    fs::create_dir(&blocked_path)?;
    let mut failures = 0;
    data.meter(false)
        .record(
            None,
            Duration::from_millis(250),
            SourceReads::default(),
            |message| {
                failures += 1;
                assert!(message.contains("retrying"));
                assert!(fs::remove_dir(&blocked_path).is_ok());
            },
        )
        .await;

    let saved = read(&data.0, "1");
    assert_eq!(failures, 1);
    assert!(!saved.enabled);
    assert_eq!(saved.off.turns, 1);
    assert_eq!(saved.off.turns_with_tokens, 0);
    assert_eq!(saved.off.working_milliseconds, 250);
    assert_eq!(saved.on.turns, 0);
    let saved_json: serde_json::Value =
        serde_json::from_slice(&fs::read(folder.join("usage.json"))?)?;
    assert_eq!(saved_json["tools"], serde_json::json!({}));
    assert!(saved_json["workspaces"].get("old").is_none());
    Ok(())
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> io::Result<Self> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?;
        let path = std::env::temp_dir().join(format!(
            "velocity-turn-usage-{}-{}",
            std::process::id(),
            stamp.as_nanos()
        ));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }

    fn meter(&self, graph_on: bool) -> UsageMeter {
        UsageMeter {
            app_data: self.0.clone(),
            project: "1".to_owned(),
            graph_on,
            worktree: self.0.clone(),
            thread: Some("thread-1".to_owned()),
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
