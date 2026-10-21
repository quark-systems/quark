//! Secrets a worker launches with that must never be written down.
//!
//! An assignment's environment is recorded in the event log, so a secret
//! such as an account's API key travels as a reference instead:
//! `QUARK_SECRET_<VAR>=keychain:<service>`. The supervisor resolves it into
//! `<VAR>` only for the session it starts ([`resolve`]).

use std::collections::BTreeMap;

use async_trait::async_trait;

/// The prefix of a variable holding a secret reference.
pub const PREFIX: &str = "QUARK_SECRET_";
/// The scheme of a Keychain reference.
pub const KEYCHAIN: &str = "keychain:";

/// Reads a secret by its Keychain service name.
#[async_trait]
pub trait Secrets: Send + Sync {
    async fn get(&self, service: &str) -> Result<String, String>;
}

/// The reference variable for `var`'s secret in `service`.
pub fn reference(var: &str, service: &str) -> (String, String) {
    (format!("{PREFIX}{var}"), format!("{KEYCHAIN}{service}"))
}

/// `env` with every secret reference replaced by its secret.
pub async fn resolve(
    env: BTreeMap<String, String>,
    secrets: Option<&dyn Secrets>,
) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    let mut refs = Vec::new();
    for (k, v) in env {
        match k.strip_prefix(PREFIX) {
            Some(var) if !var.is_empty() => refs.push((var.to_string(), v)),
            _ => {
                out.insert(k, v);
            }
        }
    }
    for (var, v) in refs {
        let service = v
            .strip_prefix(KEYCHAIN)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("{PREFIX}{var} is not a keychain reference"))?;
        let secrets = secrets.ok_or_else(|| format!("no secret store to read {var} from"))?;
        let secret = secrets
            .get(service)
            .await
            .map_err(|e| format!("reading {var}: {e}"))?;
        out.insert(var, secret);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct One;

    #[async_trait]
    impl Secrets for One {
        async fn get(&self, service: &str) -> Result<String, String> {
            match service {
                "quark/acc_1" => Ok("sk-1".into()),
                _ => Err("no such item".into()),
            }
        }
    }

    #[tokio::test]
    async fn references_become_the_secret_only_at_launch() {
        let (k, v) = reference("BOB_API_KEY", "quark/acc_1");
        let env = BTreeMap::from([(k, v), ("A".to_string(), "1".to_string())]);
        let out = resolve(env.clone(), Some(&One)).await.unwrap();
        assert_eq!(
            out,
            BTreeMap::from([
                ("A".to_string(), "1".to_string()),
                ("BOB_API_KEY".to_string(), "sk-1".to_string()),
            ])
        );
        assert!(resolve(env, None).await.is_err());
        let missing = BTreeMap::from([reference("K", "quark/acc_2")]);
        assert!(resolve(missing, Some(&One))
            .await
            .unwrap_err()
            .contains("no such item"));
    }
}
