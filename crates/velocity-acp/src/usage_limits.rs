//! Subscription limits read outside ACP (F15 Limit sources): Claude Code's usage report and
//! Codex's `account/rateLimits/read`. The pure half: each report becomes one reading. Its
//! headline is the window closest to its limit, since that is the one that stops work first;
//! every window the report carried is kept for the footer usage panel.

use std::collections::BTreeMap;

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
    /// Every window the report carried, shortest first (5-hour before weekly). Empty when the
    /// source names only the headline window.
    pub windows: Vec<LimitWindowReading>,
}

/// One subscription window from a usage report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LimitWindowReading {
    pub percent_used: u8,
    /// `5-hour`, `weekly`, `weekly · Opus`.
    pub window: String,
    /// The window's length, for where even spending would be (the pace mark).
    pub minutes: i64,
    pub resets_at: Option<i64>,
}

impl LimitWindowReading {
    pub(crate) fn new(
        percent_used: u8,
        window: String,
        minutes: i64,
        resets_at: Option<i64>,
    ) -> Self {
        Self {
            percent_used,
            window,
            minutes,
            resets_at,
        }
    }
}

impl LimitReading {
    /// A quota window from a report that does not include subscription plan details.
    pub fn new(percent_used: u8, window: String, resets_at: Option<i64>) -> Self {
        Self {
            percent_used,
            window,
            resets_at,
            plan: None,
            windows: Vec::new(),
        }
    }
}

const FIVE_HOURS: i64 = 5 * 60;
const ONE_DAY: i64 = 24 * 60;
const ONE_WEEK: i64 = 7 * ONE_DAY;
const THIRTY_DAYS: i64 = 30 * ONE_DAY;

/// Claude's native `rate_limit_info`, forwarded by its ACP adapter in usage metadata.
/// Utilization is a fraction here, unlike the percentages in `get_usage`.
pub fn claude_limit_event(report: &Value) -> Option<LimitReading> {
    if let Some(windows) = report.get("unifiedWindows").and_then(Value::as_object) {
        return fullest(
            windows.iter().filter_map(|(name, window)| {
                let minutes = claude_window_minutes(name)?;
                let used = window.get("utilization")?.as_f64()?;
                used.is_finite().then(|| WindowInput {
                    used: used * 100.0,
                    minutes,
                    resets_at: window.get("resetsAt").and_then(Value::as_i64),
                    label: claude_window_label(name, minutes),
                })
            }),
            None,
        );
    }
    let name = report.get("rateLimitType")?.as_str()?;
    let minutes = claude_window_minutes(name)?;
    let used = report.get("utilization")?.as_f64()?;
    if !used.is_finite() {
        return None;
    }
    fullest(
        std::iter::once(WindowInput {
            used: used * 100.0,
            minutes,
            resets_at: report.get("resetsAt").and_then(Value::as_i64),
            label: claude_window_label(name, minutes),
        }),
        None,
    )
}

/// Claude reports separate weekly windows per model family; name them so they don't read as
/// the same window twice.
fn claude_window_label(name: &str, minutes: i64) -> String {
    match name {
        "seven_day_opus" => "weekly · Opus".into(),
        "seven_day_sonnet" => "weekly · Sonnet".into(),
        _ => window_label(minutes),
    }
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
            Some(WindowInput {
                used,
                minutes,
                resets_at,
                label: window_label(minutes),
            })
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
        Some(WindowInput {
            used,
            minutes,
            resets_at,
            label: window_label(minutes),
        })
    });
    let plan = limits
        .get("planType")
        .and_then(Value::as_str)
        .map(plan_name);
    fullest(windows, plan)
}

/// One window as a report states it, before rounding.
struct WindowInput {
    used: f64,
    minutes: i64,
    resets_at: Option<i64>,
    label: String,
}

pub(crate) fn kimi_usage_reading(report: &Value) -> Option<LimitReading> {
    let mut windows: BTreeMap<i64, WindowInput> = [
        ("limit_5h", FIVE_HOURS),
        ("limit_7d", ONE_WEEK),
        // Kimi reports code usage as a breakdown of the monthly total.
        ("limit_month_total", THIRTY_DAYS),
    ]
    .into_iter()
    .filter_map(|(key, minutes)| {
        let quota = report.get("usages")?.get(key)?;
        let ratio = quota_number(quota.get("used_ratio")?)?;
        Some((minutes, kimi_window(quota, ratio, minutes)))
    })
    .collect();
    // Keep the named ratios when the report includes both forms of the same window.
    for limit in report
        .get("limits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(minutes) = kimi_window_minutes(limit)
            && let Some(window) = limit
                .get("detail")
                .and_then(|detail| counted_kimi_window(detail, minutes))
        {
            windows
                .entry(minutes)
                .and_modify(|named| named.resets_at = named.resets_at.or(window.resets_at))
                .or_insert(window);
        }
    }
    if let Some(weekly) = report
        .get("usage")
        .and_then(|summary| counted_kimi_window(summary, ONE_WEEK))
    {
        windows
            .entry(ONE_WEEK)
            .and_modify(|named| named.resets_at = named.resets_at.or(weekly.resets_at))
            .or_insert(weekly);
    }
    fullest(windows.into_values(), None)
}

fn quota_number(value: &Value) -> Option<f64> {
    let number = value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse().ok())?;
    (number.is_finite() && number >= 0.0).then_some(number)
}

fn counted_kimi_window(quota: &Value, minutes: i64) -> Option<WindowInput> {
    let limit = quota_number(quota.get("limit")?).filter(|limit| *limit > 0.0)?;
    let ratio = quota
        .get("used")
        .and_then(quota_number)
        .map(|used| used / limit)
        .or_else(|| Some(1.0 - quota_number(quota.get("remaining")?)? / limit))?;
    Some(kimi_window(quota, ratio, minutes))
}

fn kimi_window(quota: &Value, ratio: f64, minutes: i64) -> WindowInput {
    let resets_at = ["reset_time", "resetTime", "reset_at", "resetAt"]
        .into_iter()
        .filter_map(|key| quota.get(key).and_then(Value::as_str))
        .find_map(unix_seconds);
    WindowInput {
        used: ratio.clamp(0.0, 1.0) * 100.0,
        label: if minutes == THIRTY_DAYS {
            "monthly".into()
        } else {
            window_label(minutes)
        },
        minutes,
        resets_at,
    }
}

fn kimi_window_minutes(limit: &Value) -> Option<i64> {
    let window = limit.get("window")?;
    let minutes_per_unit = match window.get("timeUnit")?.as_str()? {
        "TIME_UNIT_MINUTE" => 1,
        "TIME_UNIT_HOUR" => 60,
        "TIME_UNIT_DAY" => ONE_DAY,
        "TIME_UNIT_WEEK" => ONE_WEEK,
        _ => return None,
    };
    let duration = window.get("duration")?.as_i64()?;
    duration
        .checked_mul(minutes_per_unit)
        .filter(|minutes| *minutes > 0)
}

fn fullest(
    windows: impl Iterator<Item = WindowInput>,
    plan: Option<String>,
) -> Option<LimitReading> {
    let mut windows: Vec<LimitWindowReading> = windows
        .map(|input| LimitWindowReading {
            percent_used: percent(input.used),
            window: input.label,
            minutes: input.minutes,
            resets_at: input.resets_at,
        })
        .collect();
    windows.sort_by_key(|window| window.minutes);
    let headline = windows
        .iter()
        .max_by_key(|window| window.percent_used)?
        .clone();
    Some(LimitReading {
        percent_used: headline.percent_used,
        window: headline.window,
        resets_at: headline.resets_at,
        plan,
        windows,
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
