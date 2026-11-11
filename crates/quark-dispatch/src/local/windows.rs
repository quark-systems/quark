//! Rolling usage windows (a five-hour session, a week, a model's week) and
//! the schema-5 snapshot built from them, shared by the Claude Code and
//! Codex readers.
//!
//! A scope's row is bounded by every window named for it: its
//! `effectivePercentRemaining` is the least left among them, its runway is
//! `exhausted_now` when one is spent, `projected_exhaustion` when one's
//! pace runs it out before its reset, else `through_reset`, and its
//! `spendPriority` is the cycle-weighted mean of each window's
//! [`super::spend_priority`].

use serde_json::{json, Map, Value};
use time::OffsetDateTime;

use super::{epoch, selection, tidy, utc_iso};

/// One usage window as a provider reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub id: String,
    pub label: String,
    /// `session`, `weekly`, `model`, `credits` or `unknown`.
    pub kind: String,
    pub percent_used: f64,
    pub resets_at: Option<OffsetDateTime>,
    pub window_seconds: Option<f64>,
}

impl Window {
    pub fn new(id: &str, label: &str, kind: &str, percent_used: f64) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            kind: kind.into(),
            percent_used: percent_used.clamp(0.0, 100.0),
            resets_at: None,
            window_seconds: None,
        }
    }

    pub fn resets(mut self, at: Option<OffsetDateTime>) -> Self {
        self.resets_at = at;
        self
    }

    pub fn lasting(mut self, seconds: Option<f64>) -> Self {
        self.window_seconds = seconds.filter(|s| *s > 0.0);
        self
    }

    pub fn percent_remaining(&self) -> f64 {
        100.0 - self.percent_used
    }

    /// The cycle `(start, end)` in epoch seconds, when both the reset and
    /// the window's length are known.
    fn cycle(&self) -> Option<(f64, f64)> {
        let end = epoch(self.resets_at?);
        Some((end - self.window_seconds?, end))
    }

    /// Nothing used and no clock running: the window opens on first use.
    fn untouched(&self) -> bool {
        self.percent_used == 0.0 && self.resets_at.is_none()
    }

    fn to_json(&self) -> Value {
        let mut w = json!({
            "id": self.id,
            "label": self.label,
            "kind": self.kind,
            "percentUsed": tidy(self.percent_used, 2),
            "percentRemaining": tidy(self.percent_remaining(), 2),
        });
        if let Some(at) = self.resets_at {
            w["resetsAt"] = json!(utc_iso(at));
        }
        if let Some(s) = self.window_seconds {
            w["windowSeconds"] = tidy(s, 0);
        }
        w
    }
}

/// `exhausted_now`, `projected_exhaustion`, `through_reset` or `unknown`.
fn runway(windows: &[&Window], now: f64) -> &'static str {
    if windows.iter().any(|w| w.percent_remaining() <= 0.0) {
        return "exhausted_now";
    }
    if windows.iter().all(|w| w.resets_at.is_none()) {
        return "unknown";
    }
    let mut projected = false;
    for w in windows {
        if w.untouched() {
            continue;
        }
        let Some((start, end)) = w.cycle() else {
            return "unknown";
        };
        if end <= now {
            return "unknown";
        }
        if w.percent_used == 0.0 {
            continue;
        }
        let elapsed = now - start;
        if elapsed <= 0.0 {
            return "unknown";
        }
        let burn = w.percent_used / elapsed;
        if now + w.percent_remaining() / burn < end {
            projected = true;
        }
    }
    if projected {
        "projected_exhaustion"
    } else {
        "through_reset"
    }
}

/// The cycle-weighted mean of each window's spend priority, or `None` when
/// a window cannot be measured.
fn spend_priority(windows: &[&Window], now: f64) -> Option<f64> {
    let (mut sum, mut weight) = (0.0, 0.0);
    for w in windows {
        if w.untouched() {
            continue;
        }
        let (start, end) = w.cycle()?;
        let gap = super::spend_priority(w.percent_remaining(), w.percent_used, start, end, now)?;
        sum += gap * (end - start);
        weight += end - start;
    }
    (weight > 0.0).then(|| sum / weight)
}

/// One scope's row, bounded by `windows`.
pub fn availability(scope: &str, windows: &[&Window], now: OffsetDateTime) -> Value {
    let now = epoch(now);
    let least = windows
        .iter()
        .map(|w| w.percent_remaining())
        .fold(f64::INFINITY, f64::min);
    json!({
        "scope": scope,
        "status": "known",
        "effectivePercentRemaining": tidy(least.clamp(0.0, 100.0), 2),
        "boundedBy": windows.iter().map(|w| w.id.as_str()).collect::<Vec<_>>(),
        "runway": {"status": runway(windows, now)},
        "selection": selection(spend_priority(windows, now)),
    })
}

/// A measured provider: its windows, a row per scope, and `extra` fields
/// (such as `plan`) on the provider object.
pub fn reading(
    provider: &str,
    source: &str,
    now: OffsetDateTime,
    windows: &[Window],
    scopes: &[(String, Vec<&Window>)],
    extra: Map<String, Value>,
) -> Value {
    let rows: Vec<Value> = scopes
        .iter()
        .filter(|(_, w)| !w.is_empty())
        .map(|(scope, w)| availability(scope, w, now))
        .collect();
    let status = if rows.is_empty() { "unknown" } else { "known" };
    let mut p = json!({
        "provider": provider,
        "windows": windows.iter().map(Window::to_json).collect::<Vec<_>>(),
        "state": {"status": "fresh", "stale": false, "refreshedAt": utc_iso(now)},
        "quotaSemantics": {"status": status, "effectiveAvailability": rows},
    });
    for (k, v) in extra {
        p[k] = v;
    }
    json!({
        "schemaVersion": 5,
        "generatedAt": utc_iso(now),
        "source": source,
        "providers": [p],
    })
}

/// A provider that could not be measured: `status` (`auth_required`,
/// `unavailable`, `rate_limited`, ...) and `error` say why, and `reason` is
/// a machine-readable cause such as `keychain_access_required`.
pub fn failure(
    provider: &str,
    source: &str,
    now: OffsetDateTime,
    status: &str,
    error: &str,
    reason: Option<&str>,
) -> Value {
    let mut state = json!({"status": status, "stale": false, "error": error});
    if let Some(r) = reason {
        state["reason"] = json!(r);
    }
    json!({
        "schemaVersion": 5,
        "generatedAt": utc_iso(now),
        "source": source,
        "providers": [{
            "provider": provider,
            "windows": [],
            "state": state,
            "quotaSemantics": {"status": "unknown", "effectiveAvailability": []},
        }],
    })
}

/// One line for a person: what is left per scope and window, or why the
/// provider could not be read.
pub fn status_line(v: &Value) -> String {
    let p = &v["providers"][0];
    let name = p["provider"].as_str().unwrap_or("?");
    let state = &p["state"];
    if state["status"] != "fresh" {
        let err = state["error"].as_str().unwrap_or("not measured");
        return format!("{name}: {err}");
    }
    let mut parts = Vec::new();
    for row in p["quotaSemantics"]["effectiveAvailability"]
        .as_array()
        .into_iter()
        .flatten()
    {
        parts.push(format!(
            "{} {}% left (runway {})",
            row["scope"].as_str().unwrap_or("?"),
            row["effectivePercentRemaining"],
            row["runway"]["status"].as_str().unwrap_or("unknown"),
        ));
    }
    let windows: Vec<String> = p["windows"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|w| {
            let reset = w["resetsAt"]
                .as_str()
                .map(|r| format!(", resets {r}"))
                .unwrap_or_default();
            format!(
                "{} {}%{reset}",
                w["label"].as_str().unwrap_or("?"),
                w["percentRemaining"]
            )
        })
        .collect();
    let plan = p["plan"]
        .as_str()
        .map(|p| format!(" [{p}]"))
        .unwrap_or_default();
    format!(
        "{name}{plan}: {}; windows: {}",
        parts.join(", "),
        windows.join("; ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Duration;

    fn at(s: &str) -> OffsetDateTime {
        OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).unwrap()
    }

    #[test]
    fn rows_take_the_tightest_window() {
        let now = at("2026-11-11T12:00:00Z");
        // Half the session gone, 20% used: on pace to last.
        let session = Window::new("five_hour", "session", "session", 20.0)
            .resets(Some(now + Duration::hours(2) + Duration::minutes(30)))
            .lasting(Some(18_000.0));
        // A seventh of the week gone, 60% used: runs out first.
        let week = Window::new("seven_day", "week", "weekly", 60.0)
            .resets(Some(now + Duration::days(6)))
            .lasting(Some(604_800.0));
        let row = availability("all_models", &[&session, &week], now);
        assert_eq!(row["effectivePercentRemaining"], json!(40));
        assert_eq!(row["runway"]["status"], "projected_exhaustion");
        assert!(row["selection"]["spendPriority"].as_f64().unwrap() < 0.0);

        let spent = Window::new("five_hour", "session", "session", 100.0);
        let row = availability("all_models", &[&spent, &week], now);
        assert_eq!(row["runway"]["status"], "exhausted_now");

        let row = availability("all_models", &[&session], now);
        assert_eq!(row["runway"]["status"], "through_reset");
    }

    #[test]
    fn untouched_windows_do_not_block() {
        let now = at("2026-11-11T12:00:00Z");
        let idle = Window::new("five_hour", "session", "session", 0.0).lasting(Some(18_000.0));
        let week = Window::new("seven_day", "week", "weekly", 10.0)
            .resets(Some(now + Duration::days(3)))
            .lasting(Some(604_800.0));
        let row = availability("all_models", &[&idle, &week], now);
        assert_eq!(row["runway"]["status"], "through_reset");
        assert_eq!(row["selection"]["status"], "known");
        let snap = reading(
            "claude",
            "test",
            now,
            &[idle.clone(), week.clone()],
            &[("all_models".into(), vec![&idle, &week])],
            Map::new(),
        );
        assert!(crate::quota::valid(&snap), "{snap}");
        let f = failure("claude", "test", now, "auth_required", "sign in", None);
        assert!(crate::quota::valid(&f), "{f}");
    }
}
