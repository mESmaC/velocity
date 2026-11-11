//! Google's quota report, reduced to the fullest reported window for F15's meter.

use serde_json::Value;

use crate::usage_limits::{LimitReading, unix_seconds};

/// Summary groups take precedence over the older flat buckets. Missing fractions and
/// disabled buckets are not usage readings; an empty report keeps the meter unavailable.
pub fn antigravity_usage_reading(report: &Value) -> Option<LimitReading> {
    let grouped: Vec<&Value> = report
        .get("groups")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|group| group.get("buckets").and_then(Value::as_array))
        .flatten()
        .collect();
    let buckets: Vec<&Value> = if grouped.is_empty() {
        report
            .get("buckets")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .collect()
    } else {
        grouped
    };
    fullest(buckets.into_iter().filter_map(|bucket| {
        if bucket.get("disabled").and_then(Value::as_bool) == Some(true) {
            return None;
        }
        quota_reading(bucket, bucket.get("window").and_then(Value::as_str))
    }))
}

/// Older accounts return per-model quotas without a window duration. Never infer one
/// from the reset time: a partially consumed window is shorter than its cadence.
pub fn antigravity_model_reading(report: &Value) -> Option<LimitReading> {
    let models = report.get("models")?.as_object()?;
    fullest(models.values().flat_map(|model| {
        [("quotaInfo", None), ("weeklyQuotaInfo", Some("weekly"))]
            .into_iter()
            .filter_map(move |(key, window)| quota_reading(model.get(key)?, window))
    }))
}

fn quota_reading(quota: &Value, window: Option<&str>) -> Option<LimitReading> {
    let remaining = quota.get("remainingFraction")?.as_f64()?;
    if !remaining.is_finite() {
        return None;
    }
    Some(LimitReading::new(
        ((1.0 - remaining.clamp(0.0, 1.0)) * 100.0).round() as u8,
        match window {
            Some("5h") => "5-hour",
            Some("weekly") => "weekly",
            Some(window) => window,
            None => "reported quota",
        }
        .into(),
        quota
            .get("resetTime")
            .and_then(Value::as_str)
            .and_then(unix_seconds),
    ))
}

fn fullest(readings: impl Iterator<Item = LimitReading>) -> Option<LimitReading> {
    readings.max_by_key(|reading| reading.percent_used)
}
