//! Reduce Google's quota replies to one card headline and all reported windows.

use serde_json::Value;

use crate::usage_limits::{LimitReading, LimitWindowReading, unix_seconds};

pub(crate) struct QuotaWindow {
    id: String,
    remaining: f64,
    reading: LimitWindowReading,
}

impl QuotaWindow {
    pub(crate) fn has_activity(&self) -> bool {
        self.remaining < 1.0 || self.reading.resets_at.is_some()
    }
}

/// The first host that reports an ID owns that window. The fullest window is picked
/// before rounding, so light usage still selects the right reset.
pub(crate) fn merged_reading(
    mut preferred: Vec<QuotaWindow>,
    fallback: Vec<QuotaWindow>,
) -> Option<LimitReading> {
    for window in fallback {
        if !preferred.iter().any(|existing| existing.id == window.id) {
            preferred.push(window);
        }
    }
    preferred.sort_by_key(|window| window.reading.minutes);
    let headline = &preferred
        .iter()
        .min_by(|left, right| {
            left.remaining.total_cmp(&right.remaining).then_with(|| {
                right
                    .reading
                    .resets_at
                    .is_some()
                    .cmp(&left.reading.resets_at.is_some())
            })
        })?
        .reading;
    let mut reading = LimitReading::new(
        headline.percent_used,
        headline.window.clone(),
        headline.resets_at,
    );
    reading.windows = preferred.into_iter().map(|window| window.reading).collect();
    Some(reading)
}

/// Groups take precedence over the older flat bucket list.
pub(crate) fn summary_windows(
    report: &Value,
    requested_at: i64,
) -> Result<Vec<QuotaWindow>, String> {
    let mut windows = Vec::new();
    let mut has_grouped_buckets = false;
    if let Some(groups) = optional_array(report, "groups")? {
        for group in groups {
            let Some(buckets) = optional_array(group, "buckets")? else {
                continue;
            };
            has_grouped_buckets |= !buckets.is_empty();
            let group_name = group.get("displayName").and_then(Value::as_str);
            add_buckets(&mut windows, buckets, group_name, requested_at)?;
        }
    }
    if !has_grouped_buckets && let Some(buckets) = optional_array(report, "buckets")? {
        add_buckets(&mut windows, buckets, None, requested_at)?;
    }
    if windows.is_empty() {
        return Err(format!(
            "no usable summary buckets (groups: {}, buckets: {})",
            shape(report.get("groups")),
            shape(report.get("buckets")),
        ));
    }
    Ok(windows)
}

fn add_buckets(
    windows: &mut Vec<QuotaWindow>,
    buckets: &[Value],
    group: Option<&str>,
    requested_at: i64,
) -> Result<(), String> {
    for bucket in buckets {
        if bucket.get("disabled").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let id = bucket
            .get("bucketId")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                format!(
                    "unexpected quota field bucketId ({})",
                    shape(bucket.get("bucketId"))
                )
            })?;
        let family = match id {
            id if id.starts_with("gemini-argon-") => group.unwrap_or("Gemini Argon"),
            id if id.starts_with("gemini-carbon-") => group.unwrap_or("Gemini Carbon"),
            id if id.starts_with("gemini-") => "Gemini",
            id if id.starts_with("3p-") => "Claude & GPT (shared)",
            _ => group.unwrap_or(id),
        };
        let window = bucket.get("window").and_then(Value::as_str);
        if let Some(window) = quota_window(bucket, id, family, window, Some(requested_at))? {
            windows.retain(|existing| existing.id != id);
            windows.push(window);
        }
    }
    Ok(())
}

/// Older accounts report quota by model. Keep the fullest model for each family and cadence.
pub(crate) fn model_windows(report: &Value, _: i64) -> Result<Vec<QuotaWindow>, String> {
    let models = report
        .get("models")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            format!(
                "unexpected quota field models ({})",
                shape(report.get("models"))
            )
        })?;
    let mut windows: Vec<QuotaWindow> = Vec::new();
    for (id, model) in models {
        let provider = model.get("modelProvider").and_then(Value::as_str);
        let family = match (provider, id.as_str()) {
            (Some("MODEL_PROVIDER_GOOGLE"), _) => "Gemini",
            (Some("MODEL_PROVIDER_ANTHROPIC"), _) => "Claude",
            (Some("MODEL_PROVIDER_OPENAI"), _) => "GPT",
            (_, id) if id.starts_with("gemini-") => "Gemini",
            (_, id) if id.starts_with("claude-") => "Claude",
            (_, id) if id.starts_with("gpt-") => "GPT",
            _ => continue,
        };
        for (key, cadence) in [("quotaInfo", None), ("weeklyQuotaInfo", Some("weekly"))] {
            let Some(quota) = model.get(key) else {
                continue;
            };
            let id = format!("{family}:{key}");
            if let Some(window) = quota_window(quota, &id, family, cadence, None)? {
                if let Some(existing) = windows.iter_mut().find(|existing| existing.id == id) {
                    if window.remaining < existing.remaining {
                        *existing = window;
                    }
                } else {
                    windows.push(window);
                }
            }
        }
    }
    if windows.is_empty() {
        return Err("models object contains no usable quota windows".into());
    }
    Ok(windows)
}

fn quota_window(
    quota: &Value,
    id: &str,
    family: &str,
    cadence: Option<&str>,
    requested_at: Option<i64>,
) -> Result<Option<QuotaWindow>, String> {
    let Some(fraction) = quota.get("remainingFraction") else {
        return Ok(None);
    };
    let remaining = fraction
        .as_f64()
        .ok_or_else(|| {
            format!(
                "unexpected quota field remainingFraction ({})",
                shape(Some(fraction))
            )
        })?
        .clamp(0.0, 1.0);
    let (label, minutes) = match cadence {
        Some("5h") => ("5-hour", 5 * 60),
        Some("weekly") => ("weekly", 7 * 24 * 60),
        _ => ("reported quota", 0),
    };
    let mut resets_at = quota
        .get("resetTime")
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_str()
                .and_then(unix_seconds)
                .ok_or_else(|| format!("invalid quota field resetTime ({})", shape(Some(value))))
        })
        .transpose()?;
    // An untouched summary bucket can have a rolling full-window timestamp.
    if remaining == 1.0
        && quota.get("description").is_none()
        && minutes > 0
        && let (Some(reset), Some(requested_at)) = (resets_at, requested_at)
        && reset.abs_diff(requested_at + minutes * 60) <= 60
    {
        resets_at = None;
    }
    Ok(Some(QuotaWindow {
        id: id.to_owned(),
        remaining,
        reading: LimitWindowReading::new(
            ((1.0 - remaining) * 100.0).round() as u8,
            format!("{label} · {family}"),
            minutes,
            resets_at,
        ),
    }))
}

fn optional_array<'a>(object: &'a Value, name: &str) -> Result<Option<&'a [Value]>, String> {
    object
        .get(name)
        .map(|value| {
            value
                .as_array()
                .map(Vec::as_slice)
                .ok_or_else(|| format!("unexpected quota field {name} ({})", shape(Some(value))))
        })
        .transpose()
}

fn shape(value: Option<&Value>) -> &'static str {
    // Names of expected fields and JSON types suffice to diagnose a schema change.
    match value {
        None => "missing",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    }
}

/// Parse a quota report supplied to the local diagnostic example.
pub fn antigravity_usage_reading(
    report: &Value,
    requested_at: i64,
) -> Result<LimitReading, String> {
    let windows = if report.get("models").is_some() {
        model_windows(report, requested_at)?
    } else {
        summary_windows(report, requested_at)?
    };
    merged_reading(windows, Vec::new()).ok_or_else(|| "no usable quota windows".into())
}
