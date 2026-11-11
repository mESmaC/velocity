//! Subscription limits read outside ACP (F15 Limit sources): Claude Code's usage report and
//! Codex's `account/rateLimits/read`. The pure half: each report becomes one reading, the
//! window closest to its limit, since that is the one that stops work first.

use serde_json::Value;

/// The share of one subscription window used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LimitReading {
    pub percent_used: u8,
    /// `5-hour`, `daily`, `weekly`, or the window's length.
    pub window: String,
    /// When the window resets, in Unix seconds, when the provider says.
    pub resets_at: Option<i64>,
    /// The subscription's plan as the provider names it (`Max`, `Pro`).
    pub plan: Option<String>,
}

const FIVE_HOURS: i64 = 5 * 60;
const ONE_DAY: i64 = 24 * 60;
const ONE_WEEK: i64 = 7 * ONE_DAY;

/// Claude's native `rate_limit_info`, forwarded by its ACP adapter in usage metadata.
/// Utilization is a fraction here, unlike the percentages in `get_usage`.
pub fn claude_limit_event(report: &Value) -> Option<LimitReading> {
    if let Some(windows) = report.get("unifiedWindows").and_then(Value::as_object) {
        return fullest(
            windows.iter().filter_map(|(name, window)| {
                let minutes = claude_window_minutes(name)?;
                let used = window.get("utilization")?.as_f64()?;
                used.is_finite().then_some((
                    used * 100.0,
                    minutes,
                    window.get("resetsAt").and_then(Value::as_i64),
                ))
            }),
            None,
        );
    }
    let minutes = claude_window_minutes(report.get("rateLimitType")?.as_str()?)?;
    let used = report.get("utilization")?.as_f64()?;
    if !used.is_finite() {
        return None;
    }
    fullest(
        std::iter::once((
            used * 100.0,
            minutes,
            report.get("resetsAt").and_then(Value::as_i64),
        )),
        None,
    )
}

fn claude_window_minutes(name: &str) -> Option<i64> {
    match name {
        "five_hour" => Some(FIVE_HOURS),
        "seven_day" | "seven_day_opus" | "seven_day_sonnet" => Some(ONE_WEEK),
        _ => None,
    }
}

/// Claude Code's `get_usage` control response (`response.response`). `None` when it reports
/// no rate limits, which shows as "limit not reported".
pub fn claude_usage_reading(report: &Value) -> Option<LimitReading> {
    if report.get("rate_limits_available").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let limits = report.get("rate_limits")?;
    let windows = [("five_hour", FIVE_HOURS), ("seven_day", ONE_WEEK)]
        .into_iter()
        .filter_map(|(key, minutes)| {
            let window = limits.get(key)?;
            let used = window.get("utilization").and_then(Value::as_f64)?;
            let resets_at = window
                .get("resets_at")
                .and_then(Value::as_str)
                .and_then(unix_seconds);
            Some((used, minutes, resets_at))
        });
    let plan = report
        .get("subscription_type")
        .and_then(Value::as_str)
        .map(plan_name);
    fullest(windows, plan)
}

/// Codex's `account/rateLimits/read` result.
pub fn codex_usage_reading(result: &Value) -> Option<LimitReading> {
    let limits = result.get("rateLimits")?;
    let windows = ["primary", "secondary"].into_iter().filter_map(|key| {
        let window = limits.get(key)?;
        let used = window.get("usedPercent").and_then(Value::as_f64)?;
        let minutes = window
            .get("windowDurationMins")
            .and_then(Value::as_i64)
            .unwrap_or(FIVE_HOURS);
        let resets_at = window.get("resetsAt").and_then(Value::as_i64);
        Some((used, minutes, resets_at))
    });
    let plan = limits
        .get("planType")
        .and_then(Value::as_str)
        .map(plan_name);
    fullest(windows, plan)
}

fn fullest(
    windows: impl Iterator<Item = (f64, i64, Option<i64>)>,
    plan: Option<String>,
) -> Option<LimitReading> {
    let (used, minutes, resets_at) = windows.max_by(|left, right| left.0.total_cmp(&right.0))?;
    Some(LimitReading {
        percent_used: percent(used),
        window: window_label(minutes),
        resets_at,
        plan,
    })
}

fn percent(used: f64) -> u8 {
    if !used.is_finite() {
        return 0;
    }
    // Clamped to 0–100 first, so the cast never truncates.
    used.round().clamp(0.0, 100.0) as u8
}

/// `5-hour`, `daily`, `weekly`, else the window's length in hours or days.
pub fn window_label(minutes: i64) -> String {
    match minutes {
        FIVE_HOURS => "5-hour".into(),
        ONE_DAY => "daily".into(),
        ONE_WEEK => "weekly".into(),
        minutes if minutes % ONE_DAY == 0 && minutes > 0 => format!("{}-day", minutes / ONE_DAY),
        minutes if minutes >= 60 => format!("{}-hour", minutes / 60),
        minutes => format!("{minutes}-minute"),
    }
}

/// `max` → `Max`, `pro` → `Pro`; names that already have capitals stay as they are.
fn plan_name(plan: &str) -> String {
    let mut characters = plan.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

/// Unix seconds from an RFC 3339 time such as `2026-11-13T03:59:59.007828+00:00`.
pub fn unix_seconds(text: &str) -> Option<i64> {
    let (date, rest) = text.split_once('T')?;
    let mut date_parts = date.splitn(3, '-').map(str::parse::<i64>);
    let (year, month, day) = (
        date_parts.next()?.ok()?,
        date_parts.next()?.ok()?,
        date_parts.next()?.ok()?,
    );
    let time_end = rest.find(['Z', 'z', '+', '-']).unwrap_or(rest.len());
    let (time, zone) = rest.split_at(time_end);
    let mut time_parts = time.split(':');
    let hours: i64 = time_parts.next()?.parse().ok()?;
    let minutes: i64 = time_parts.next()?.parse().ok()?;
    let seconds: i64 = time_parts
        .next()
        .map(|seconds| seconds.split('.').next().unwrap_or("0").parse())
        .unwrap_or(Ok(0))
        .ok()?;
    let local = days_from_civil(year, month, day) * 86_400 + hours * 3_600 + minutes * 60 + seconds;
    Some(local - zone_offset_seconds(zone)?)
}

fn zone_offset_seconds(zone: &str) -> Option<i64> {
    if zone.is_empty() || zone.eq_ignore_ascii_case("z") {
        return Some(0);
    }
    let sign = if zone.starts_with('-') { -1 } else { 1 };
    let (hours, minutes) = zone
        .get(1..)?
        .split_once(':')
        .unwrap_or((zone.get(1..)?, "0"));
    Some(sign * (hours.parse::<i64>().ok()? * 3_600 + minutes.parse::<i64>().ok()? * 60))
}

/// Days since 1970-01-01 in the proleptic Gregorian calendar (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}
