//! Checking a model against a harness's own catalog (`models.list`), for
//! harnesses such as Kiro that silently start their default model on an id
//! they do not know.

use std::time::Duration;

use quark_core::harness::HarnessManifest;
use serde_json::Value;

/// How long the catalog command may take.
pub const TIMEOUT: Duration = Duration::from_secs(20);

/// What the catalog says about one model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    /// Listed, or the harness has no catalog command.
    Listed,
    /// The catalog was read and does not list it; the ids it does list.
    Unlisted(Vec<String>),
    /// The catalog could not be read, so nothing is known.
    Unreadable(String),
}

/// Look `model` up in `m`'s catalog.
pub async fn check(m: &HarnessManifest, model: &str, timeout: Duration) -> Listing {
    let Some((exe, args)) = m.models.list.split_first() else {
        return Listing::Listed;
    };
    let out = tokio::time::timeout(
        timeout,
        tokio::process::Command::new(exe)
            .args(args)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await;
    let out = match out {
        Err(_) => {
            return Listing::Unreadable(format!(
                "`{}` did not answer within {}s",
                m.models.list.join(" "),
                timeout.as_secs()
            ))
        }
        Ok(Err(e)) => return Listing::Unreadable(format!("{exe}: {e}")),
        Ok(Ok(o)) => o,
    };
    match ids(&String::from_utf8_lossy(&out.stdout)) {
        None => Listing::Unreadable(format!(
            "`{}` printed no readable catalog (exit {})",
            m.models.list.join(" "),
            out.status
        )),
        Some(ids) if ids.iter().any(|i| i == model) => Listing::Listed,
        Some(ids) => Listing::Unlisted(ids),
    }
}

/// The model ids in a catalog: `models` (or the top level) as a list of
/// objects with `model_id` or `id`, or of strings. `None` when there are
/// none, which proves nothing.
pub fn ids(json: &str) -> Option<Vec<String>> {
    let v: Value = serde_json::from_str(json.trim()).ok()?;
    let list = v.get("models").unwrap_or(&v).as_array()?;
    let ids: Vec<String> = list
        .iter()
        .filter_map(|m| {
            m.as_str()
                .or_else(|| m.get("model_id").and_then(Value::as_str))
                .or_else(|| m.get("id").and_then(Value::as_str))
                .map(str::to_string)
        })
        .collect();
    (!ids.is_empty()).then_some(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ManifestRegistry;

    #[test]
    fn reads_catalog_shapes() {
        assert_eq!(
            ids(r#"{"models":[{"model_id":"claude-sonnet-4.6","name":"x"},{"model_id":"auto"}]}"#),
            Some(vec!["claude-sonnet-4.6".into(), "auto".into()])
        );
        assert_eq!(
            ids(r#"["a", {"id": "b"}]"#),
            Some(vec!["a".into(), "b".into()])
        );
        assert_eq!(ids(r#"{"models":[]}"#), None);
        assert_eq!(ids("not json"), None);
    }

    #[tokio::test]
    async fn checks_against_the_command() {
        let reg = ManifestRegistry::builtin();
        assert_eq!(
            check(reg.get("claude-code").unwrap(), "opus", TIMEOUT).await,
            Listing::Listed
        );
        let mut kiro = (**reg.get("kiro").unwrap()).clone();
        kiro.models.list = vec![
            "sh".into(),
            "-c".into(),
            r#"echo '{"models":[{"model_id":"auto"},{"model_id":"claude-sonnet-4.6"}]}'"#.into(),
        ];
        assert_eq!(check(&kiro, "auto", TIMEOUT).await, Listing::Listed);
        assert_eq!(
            check(&kiro, "gpt-9", TIMEOUT).await,
            Listing::Unlisted(vec!["auto".into(), "claude-sonnet-4.6".into()])
        );
        kiro.models.list = vec!["sh".into(), "-c".into(), "echo nope; exit 3".into()];
        assert!(matches!(
            check(&kiro, "auto", TIMEOUT).await,
            Listing::Unreadable(_)
        ));
        kiro.models.list = vec!["/no/such/kiro-cli".into()];
        assert!(matches!(
            check(&kiro, "auto", TIMEOUT).await,
            Listing::Unreadable(_)
        ));
        kiro.models.list = vec!["sleep".into(), "5".into()];
        assert!(matches!(
            check(&kiro, "auto", Duration::from_millis(100)).await,
            Listing::Unreadable(_)
        ));
    }
}
