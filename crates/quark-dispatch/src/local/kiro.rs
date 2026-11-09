//! Kiro CLI's plan usage, from its own `/usage` (firstmate's
//! `fm-kiro-quota.sh`).
//!
//! `kiro-cli chat --v2 --no-interactive /usage` prints the plan's credit
//! usage without a model call and without spending credits:
//!
//! ```text
//! Estimated Usage | resets on 2026-11-01 | KIRO PRO
//! Credits (3.69 of 1000 covered in plan), 0.4%
//! ```
//!
//! The engine is always pinned with `--v2`: under `--v3` the same text is
//! not a slash command but a prompt the model answers, which spends
//! credits. The read runs in a fixed probe directory (Kiro records one empty
//! session for it per read) with stdin closed and a hard bound, generous
//! because a `KIRO_API_KEY` sign-in makes `/usage` take tens of seconds.
//!
//! The default login is the key Quark keeps for it in the Keychain when
//! there is one ([`KiroUsage::secrets`]), else whatever the daemon's own
//! environment signs in with (`KIRO_API_KEY` or `kiro-cli login`). So a
//! quarkd launchd started, without the person's shell, still reads it.
//!
//! The reading is one `all_models` row: the plan credits not yet used as
//! `effectivePercentRemaining`, quota-axi's `spendPriority` over the month
//! ending at the reset (00:00 UTC on the printed date), and an `overage`
//! object. Overage is allowed when `kiro.json` says so or when `/usage`
//! shows use past the plan; then the row never reads exhausted and its
//! runway is `through_reset`. Otherwise a spent plan is `exhausted_now`, and
//! a month's pace that runs out before the reset is `projected_exhaustion`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use time::{Date, Month, OffsetDateTime, Time};

use super::{epoch, leading_number, run_with, selection, snapshot, strip_ansi, tidy, utc_iso};

pub const PROVIDER: &str = "kiro";
/// The variable that signs Kiro CLI in with an API key.
pub const KEY_ENV: &str = "KIRO_API_KEY";
const SOURCE: &str = "kiro-cli chat --v2 --no-interactive /usage";

/// `<home>/quota/kiro.json`. Absent means overage is not allowed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KiroConfig {
    /// The plan keeps serving, and billing, past its covered credits.
    #[serde(default)]
    pub overage_allowed: bool,
}

/// Reads Kiro's `/usage`.
#[derive(Clone)]
pub struct KiroUsage {
    pub bin: String,
    /// Where `/usage` runs.
    pub probe_dir: PathBuf,
    /// `kiro.json`; a missing file is the defaults.
    pub config: PathBuf,
    pub timeout: Duration,
    /// Where the default login's API key may be kept, and its service name.
    /// A key found there signs `/usage` in; none leaves the environment's.
    pub secrets: Option<(Arc<dyn quark_supervisor::Secrets>, String)>,
}

impl std::fmt::Debug for KiroUsage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KiroUsage")
            .field("bin", &self.bin)
            .field("probe_dir", &self.probe_dir)
            .field("config", &self.config)
            .field("timeout", &self.timeout)
            .field("key_service", &self.secrets.as_ref().map(|(_, s)| s))
            .finish()
    }
}

impl KiroUsage {
    /// The reader for a Quark home: config in `<home>/quota/kiro.json`, the
    /// probe in `<home>/run/kiro-usage-probe`.
    pub fn for_home(home: &std::path::Path) -> Self {
        Self {
            bin: "kiro-cli".into(),
            probe_dir: home.join("run").join("kiro-usage-probe"),
            config: home.join("quota").join("kiro.json"),
            // With KIRO_API_KEY set, /usage takes 20-30s (2s on an SSO
            // sign-in alone), and was seen past 45s on 2026-11-09.
            timeout: Duration::from_secs(75),
            secrets: None,
        }
    }

    /// Sign the default login's reads in with the key kept in `secrets`
    /// under `service`, when there is one.
    pub fn with_secrets(
        mut self,
        secrets: Arc<dyn quark_supervisor::Secrets>,
        service: impl Into<String>,
    ) -> Self {
        self.secrets = Some((secrets, service.into()));
        self
    }

    /// The default login's kept key, if any.
    async fn kept_key(&self) -> Option<String> {
        let (secrets, service) = self.secrets.as_ref()?;
        secrets
            .get(service)
            .await
            .ok()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
    }

    fn load_config(&self) -> Result<KiroConfig, String> {
        match std::fs::read_to_string(&self.config) {
            Ok(s) => serde_json::from_str(&s)
                .map_err(|e| format!("{} is not valid: {e}", self.config.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(KiroConfig::default()),
            Err(e) => Err(format!("{}: {e}", self.config.display())),
        }
    }

    async fn usage_text(&self, key: Option<&str>) -> Result<String, String> {
        std::fs::create_dir_all(&self.probe_dir)
            .map_err(|e| format!("cannot create {}: {e}", self.probe_dir.display()))?;
        let argv: Vec<String> = [&self.bin, "chat", "--v2", "--no-interactive", "/usage"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let env: Vec<(&str, &str)> = key.map(|k| (KEY_ENV, k)).into_iter().collect();
        // A failed exit can still print the usage lines; the parse decides.
        run_with(&argv, Some(&self.probe_dir), &env, self.timeout)
            .await
            .map(|(_, text)| text)
    }

    async fn read_signed_in(&self, key: Option<&str>) -> Value {
        let now = OffsetDateTime::now_utc();
        let config = self.load_config();
        let text = match &config {
            Err(_) => Ok(String::new()),
            Ok(_) => self.usage_text(key).await,
        };
        reading(config, text, now)
    }
}

#[async_trait]
impl super::LocalReading for KiroUsage {
    fn provider(&self) -> &str {
        PROVIDER
    }

    async fn read(&self) -> Value {
        let key = self.kept_key().await;
        self.read_signed_in(key.as_deref()).await
    }

    /// Kiro's plan usage for an account that signs in with `key`.
    async fn read_with_key(&self, key: &str) -> Option<Value> {
        Some(self.read_signed_in(Some(key.trim())).await)
    }
}

/// The parsed lines of `/usage`.
#[derive(Debug, Clone, PartialEq)]
struct Usage {
    used: f64,
    plan: f64,
    resets: Date,
    plan_name: Option<String>,
}

fn parse_usage(text: &str) -> Option<Usage> {
    let text = strip_ansi(text);
    let mut credits = None;
    let mut reset = None;
    for line in text.lines() {
        if let Some(i) = line.find("resets on") {
            let rest = line[i + "resets on".len()..].trim_start();
            if let Some(date) = rest.get(..10).and_then(parse_date) {
                let plan_name = rest[10..]
                    .trim_start()
                    .strip_prefix('|')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty() && !s.contains('|'));
                reset = Some((date, plan_name));
            }
        }
        if let Some(i) = line.find('(') {
            let inner = &line[i + 1..];
            if let Some((used, rest)) = leading_number(inner) {
                if let Some(rest) = rest.trim_start().strip_prefix("of") {
                    if let Some((plan, rest)) = leading_number(rest) {
                        let rest = rest.trim_start();
                        if rest.starts_with("covered in plan")
                            && rest["covered in plan".len()..]
                                .trim_start()
                                .starts_with(')')
                        {
                            credits = Some((used, plan));
                        }
                    }
                }
            }
        }
    }
    let ((used, plan), (resets, plan_name)) = (credits?, reset?);
    Some(Usage {
        used,
        plan,
        resets,
        plan_name,
    })
}

fn parse_date(s: &str) -> Option<Date> {
    let mut parts = s.split('-');
    let y: i32 = parts.next()?.parse().ok()?;
    let m: u8 = parts.next()?.parse().ok()?;
    let d: u8 = parts.next()?.parse().ok()?;
    Date::from_calendar_date(y, Month::try_from(m).ok()?, d).ok()
}

/// The same day one calendar month earlier, clamped to the month's length.
fn month_before(day: Date) -> Date {
    let (y, m) = match day.month() {
        Month::January => (day.year() - 1, Month::December),
        m => (day.year(), m.previous()),
    };
    let last = m.length(y);
    Date::from_calendar_date(y, m, day.day().min(last)).unwrap_or(day)
}

fn reading(
    config: Result<KiroConfig, String>,
    text: Result<String, String>,
    now: OffsetDateTime,
) -> Value {
    let mut kiro = json!({"confidence": "unknown", "generatedAt": utc_iso(now), "source": SOURCE});
    let unknown = |kiro: Value| snapshot(PROVIDER, "quarkd", now, "unknown", vec![], kiro);
    let cfg = match config {
        Ok(c) => c,
        Err(e) => {
            kiro["reason"] = json!(e);
            return unknown(kiro);
        }
    };
    let text = match text {
        Ok(t) => t,
        Err(e) => {
            kiro["reason"] = json!(e);
            return unknown(kiro);
        }
    };
    let Some(u) = parse_usage(&text) else {
        kiro["reason"] = json!("no recognizable credit line in /usage output");
        let clean = strip_ansi(&text);
        let lines: Vec<&str> = clean
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        if let Some(last) = lines.last() {
            // The last line is usually why, such as a lapsed sign-in.
            kiro["reason"] = json!(format!(
                "no recognizable credit line in /usage output, which ended: {last}"
            ));
            kiro["output"] = json!(lines[lines.len().saturating_sub(3)..]);
        }
        return unknown(kiro);
    };
    if u.plan <= 0.0 {
        kiro["reason"] = json!("the plan reports no covered credits");
        return unknown(kiro);
    }
    let end = u.resets.with_time(Time::MIDNIGHT).assume_utc();
    let start = month_before(u.resets)
        .with_time(Time::MIDNIGHT)
        .assume_utc();
    let (start_s, end_s, now_s) = (epoch(start), epoch(end), epoch(now));
    let over = (u.used - u.plan).max(0.0);
    let allowed = cfg.overage_allowed || over > 0.0;
    let pct_used = 100.0 * u.used / u.plan;
    let pct = ((100.0 - pct_used).clamp(0.0, 100.0) * 100.0).round() / 100.0;
    let sp = super::spend_priority(pct, pct_used, start_s, end_s, now_s);
    let runway = if allowed {
        json!({"status": "through_reset"})
    } else if pct <= 0.0 {
        json!({"status": "exhausted_now"})
    } else {
        let elapsed = now_s - start_s;
        let mut r = json!({"status": "through_reset"});
        if u.used > 0.0 && elapsed > 0.0 {
            let left = (u.plan - u.used) / (u.used / elapsed);
            if now_s + left < end_s {
                let at = now + time::Duration::seconds_f64(left);
                r = json!({"status": "projected_exhaustion", "usableRunwaySeconds": left as i64,
                           "projectedExhaustedAt": utc_iso(at)});
            }
        }
        r
    };
    let overage =
        json!({"allowed": allowed, "active": over > 0.0, "creditsOverPlan": tidy(over, 4)});
    let row = json!({
        "scope": "all_models",
        "status": "known",
        "effectivePercentRemaining": tidy(pct, 2),
        "runway": runway,
        "selection": selection(sp),
        "overage": overage,
    });
    kiro = json!({
        "confidence": "known",
        "generatedAt": utc_iso(now),
        "source": SOURCE,
        "usedCredits": tidy(u.used, 4),
        "planCredits": tidy(u.plan, 4),
        "plan": u.plan_name,
        "periodStart": utc_iso(start),
        "resetsAt": utc_iso(end),
        "overage": overage,
        "reason": "plan usage read",
    });
    snapshot(PROVIDER, "quarkd", now, "known", vec![row], kiro)
}

/// One line for people: usage, reset and overage, or why it is unmeasured.
pub fn status_line(reading: &Value) -> String {
    let p = &reading["providers"][0];
    let k = &p[PROVIDER];
    if p["quotaSemantics"]["status"] != "known" {
        return format!(
            "kiro: unmeasured - {}",
            k["reason"].as_str().unwrap_or("unknown")
        );
    }
    let o = &k["overage"];
    let note = if o["active"] == true {
        format!("in overage, {} credits over plan", o["creditsOverPlan"])
    } else if o["allowed"] == true {
        "overage allowed, not active".into()
    } else {
        "overage not allowed".into()
    };
    format!(
        "kiro: {} of {} plan credits used ({}% left), resets {}; {note}",
        k["usedCredits"],
        k["planCredits"],
        p["quotaSemantics"]["effectiveAvailability"][0]["effectivePercentRemaining"],
        k["resetsAt"].as_str().unwrap_or("").get(..10).unwrap_or("")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::LocalReading;
    use crate::QuotaSnapshot;

    const USAGE: &str = "\x1b[1mEstimated Usage\x1b[0m | resets on 2026-11-01 | KIRO PRO\n\
                         Credits (250 of 1,000 covered in plan), 25%\n";

    fn at(s: &str) -> OffsetDateTime {
        OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).unwrap()
    }

    fn row(v: &Value) -> &Value {
        &v["providers"][0]["quotaSemantics"]["effectiveAvailability"][0]
    }

    #[test]
    fn reads_plan_usage() {
        let now = at("2026-10-16T12:00:00Z");
        let v = reading(Ok(KiroConfig::default()), Ok(USAGE.into()), now);
        let snap = QuotaSnapshot::parse_checked(&v.to_string()).unwrap();
        let p = snap.provider("kiro").unwrap();
        assert!(p.measured());
        assert_eq!(p.rows[0].percent_remaining, Some(75.0));
        assert!(!p.rows[0].overage_allowed);
        assert_eq!(row(&v)["runway"]["status"], "through_reset");
        let k = &v["providers"][0]["kiro"];
        assert_eq!(k["plan"], "KIRO PRO");
        assert_eq!(k["periodStart"], "2026-10-01T00:00:00Z");
        assert!(p.rows[0].spend_priority.unwrap() > 0.0);
        assert_eq!(
            status_line(&v),
            "kiro: 250 of 1000 plan credits used (75% left), resets 2026-11-01; overage not allowed"
        );
    }

    #[test]
    fn pace_and_exhaustion_without_overage() {
        // 90% used a third of the way in runs out before the reset.
        let fast = "resets on 2026-11-01 | KIRO PRO\nCredits (900 of 1000 covered in plan), 90%\n";
        let v = reading(
            Ok(KiroConfig::default()),
            Ok(fast.into()),
            at("2026-10-11T00:00:00Z"),
        );
        assert_eq!(row(&v)["runway"]["status"], "projected_exhaustion");
        let spent = "resets on 2026-11-01\nCredits (1000 of 1000 covered in plan), 100%\n";
        let v = reading(
            Ok(KiroConfig::default()),
            Ok(spent.into()),
            at("2026-10-11T00:00:00Z"),
        );
        assert_eq!(row(&v)["runway"]["status"], "exhausted_now");
        assert_eq!(row(&v)["effectivePercentRemaining"], 0);
    }

    #[test]
    fn overage_is_a_budget_not_a_block() {
        let over = "resets on 2026-11-01\nCredits (1,100 of 1000 covered in plan), 110%\n";
        let v = reading(
            Ok(KiroConfig::default()),
            Ok(over.into()),
            at("2026-10-16T00:00:00Z"),
        );
        let snap = QuotaSnapshot::parse_checked(&v.to_string()).unwrap();
        let r = &snap.provider("kiro").unwrap().rows[0];
        assert!(r.overage_allowed && r.overage_active);
        assert_eq!(r.runway.as_deref(), Some("through_reset"));
        assert!(r.spend_priority.unwrap() < 0.0);
        assert!(status_line(&v).ends_with("in overage, 100 credits over plan"));

        let cfg = KiroConfig {
            overage_allowed: true,
        };
        let spent = "resets on 2026-11-01\nCredits (1000 of 1000 covered in plan), 100%\n";
        let v = reading(Ok(cfg), Ok(spent.into()), at("2026-10-16T00:00:00Z"));
        assert_eq!(row(&v)["runway"]["status"], "through_reset");
        assert_eq!(row(&v)["overage"]["active"], false);
    }

    #[test]
    fn anything_unreadable_is_unmeasured() {
        let now = at("2026-10-16T00:00:00Z");
        for v in [
            reading(Ok(KiroConfig::default()), Ok("Not logged in\n".into()), now),
            reading(
                Ok(KiroConfig::default()),
                Err("kiro-cli: not found".into()),
                now,
            ),
            reading(Err("kiro.json is not valid".into()), Ok(USAGE.into()), now),
        ] {
            let snap = QuotaSnapshot::parse_checked(&v.to_string()).unwrap();
            assert!(!snap.provider("kiro").unwrap().measured());
            assert!(status_line(&v).starts_with("kiro: unmeasured - "));
        }
        let v = reading(
            Ok(KiroConfig::default()),
            Ok("a\nb\nc\nNot logged in\n".into()),
            now,
        );
        assert_eq!(
            v["providers"][0]["kiro"]["output"],
            json!(["b", "c", "Not logged in"])
        );
        // Dispatch's verdict carries the reader's reason.
        let snap = QuotaSnapshot::parse_checked(&v.to_string()).unwrap();
        assert_eq!(
            snap.provider("kiro").unwrap().reason.as_deref(),
            Some("no recognizable credit line in /usage output, which ended: Not logged in")
        );
    }

    #[test]
    fn month_before_clamps() {
        let d = |y, m, d| Date::from_calendar_date(y, Month::try_from(m).unwrap(), d).unwrap();
        assert_eq!(month_before(d(2026, 3, 31)), d(2026, 2, 28));
        assert_eq!(month_before(d(2026, 1, 15)), d(2025, 12, 15));
    }

    #[test]
    fn config_rejects_unknown_keys() {
        let dir = std::env::temp_dir().join(format!("kiro-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut r = KiroUsage::for_home(&dir);
        assert_eq!(r.load_config(), Ok(KiroConfig::default()));
        r.config = dir.join("kiro.json");
        std::fs::write(&r.config, r#"{"overage_alowed": true}"#).unwrap();
        assert!(r.load_config().is_err());
        std::fs::write(&r.config, r#"{"overage_allowed": true}"#).unwrap();
        assert!(r.load_config().unwrap().overage_allowed);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A fake `kiro-cli` that prints usage only when signed in with `sk-kept`.
    #[cfg(unix)]
    fn fake_kiro(dir: &std::path::Path) -> String {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("kiro-cli");
        std::fs::write(
            &bin,
            "#!/bin/sh\nif [ \"$KIRO_API_KEY\" = sk-kept ]; then\n\
             echo 'Estimated Usage | resets on 2099-11-01 | KIRO PRO'\n\
             echo 'Credits (250 of 1,000 covered in plan), 25%'\n\
             else echo 'Not logged in.'; fi\n",
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin.display().to_string()
    }

    struct Kept(Option<&'static str>);

    #[async_trait]
    impl quark_supervisor::Secrets for Kept {
        async fn get(&self, service: &str) -> Result<String, String> {
            assert_eq!(service, "quark/accounts/default-kiro");
            self.0
                .map(String::from)
                .ok_or_else(|| "no such item".into())
        }
    }

    fn measured(v: &Value) -> bool {
        v["providers"][0]["quotaSemantics"]["status"] == "known"
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_kept_key_signs_the_default_read_in() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = KiroUsage::for_home(dir.path());
        r.bin = fake_kiro(dir.path());
        assert!(!measured(&r.read().await), "no key anywhere");
        let kept = r.clone().with_secrets(
            Arc::new(Kept(Some("sk-kept\n"))),
            "quark/accounts/default-kiro",
        );
        assert!(measured(&kept.read().await));
        let missing = r
            .clone()
            .with_secrets(Arc::new(Kept(None)), "quark/accounts/default-kiro");
        assert!(!measured(&missing.read().await));
        let keyed = r.read_with_key("sk-kept").await.unwrap();
        assert!(measured(&keyed));
    }
}
