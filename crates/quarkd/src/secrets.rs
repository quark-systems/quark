//! Secrets quarkd keeps for accounts: API keys, in the macOS Keychain.
//!
//! A key never reaches the database or the event log. An account records
//! only its Keychain service name, and a worker's environment carries a
//! reference the supervisor resolves when it starts the session
//! ([`quark_supervisor::secrets`]).

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;

/// The Keychain account name every item is stored under.
const KEYCHAIN_ACCOUNT: &str = "quark";
/// Longest a Keychain call may take.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Where account secrets are kept.
#[async_trait]
pub trait SecretStore: quark_supervisor::Secrets {
    /// Store `secret` under `service`, replacing any earlier one.
    async fn set(&self, service: &str, secret: &str) -> Result<(), String>;
    /// Remove `service`'s secret; absent is fine.
    async fn delete(&self, service: &str) -> Result<(), String>;
}

/// The login Keychain, through `security`.
#[derive(Debug, Clone)]
pub struct Keychain {
    pub bin: String,
}

impl Default for Keychain {
    fn default() -> Self {
        Self {
            bin: "/usr/bin/security".into(),
        }
    }
}

impl Keychain {
    async fn run(&self, args: &[&str], stdin: Option<String>) -> Result<String, String> {
        let mut cmd = tokio::process::Command::new(&self.bin);
        cmd.args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("the Keychain is not available here ({}: {e})", self.bin))?;
        if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
            pipe.write_all(text.as_bytes())
                .await
                .map_err(|e| e.to_string())?;
        }
        let out = tokio::time::timeout(TIMEOUT, child.wait_with_output())
            .await
            .map_err(|_| "the Keychain did not answer".to_string())?
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(err
                .lines()
                .next()
                .unwrap_or("Keychain call failed")
                .trim()
                .into());
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

#[async_trait]
impl quark_supervisor::Secrets for Keychain {
    async fn get(&self, service: &str) -> Result<String, String> {
        let out = self
            .run(
                &[
                    "find-generic-password",
                    "-a",
                    KEYCHAIN_ACCOUNT,
                    "-s",
                    service,
                    "-w",
                ],
                None,
            )
            .await?;
        let secret = out.trim_end_matches(['\r', '\n']).to_string();
        if secret.is_empty() {
            return Err(format!("no Keychain item for {service}"));
        }
        Ok(secret)
    }
}

#[async_trait]
impl SecretStore for Keychain {
    async fn set(&self, service: &str, secret: &str) -> Result<(), String> {
        check_plain(service)?;
        check_plain(secret)?;
        // Through `security -i` on stdin, so the secret is never on a
        // command line other processes can read.
        let line = format!(
            "add-generic-password -U -a {KEYCHAIN_ACCOUNT} -s \"{service}\" -w \"{secret}\"\n"
        );
        self.run(&["-i"], Some(line)).await.map(|_| ())
    }

    async fn delete(&self, service: &str) -> Result<(), String> {
        match self
            .run(
                &[
                    "delete-generic-password",
                    "-a",
                    KEYCHAIN_ACCOUNT,
                    "-s",
                    service,
                ],
                None,
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(e) if e.contains("could not be found") => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// Text safe inside a `security -i` double-quoted argument.
fn check_plain(s: &str) -> Result<(), String> {
    if s.is_empty()
        || s.chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '"' || c == '\\')
    {
        return Err("must be non-empty with no spaces, quotes or backslashes".into());
    }
    Ok(())
}

/// Secrets in memory, for tests.
#[derive(Debug, Default)]
pub struct MemorySecrets {
    items: Mutex<HashMap<String, String>>,
}

impl MemorySecrets {
    pub fn new() -> Self {
        Self::default()
    }

    /// The service names held.
    pub fn services(&self) -> Vec<String> {
        let mut s: Vec<String> = self.items.lock().unwrap().keys().cloned().collect();
        s.sort();
        s
    }
}

#[async_trait]
impl quark_supervisor::Secrets for MemorySecrets {
    async fn get(&self, service: &str) -> Result<String, String> {
        self.items
            .lock()
            .unwrap()
            .get(service)
            .cloned()
            .ok_or_else(|| format!("no Keychain item for {service}"))
    }
}

#[async_trait]
impl SecretStore for MemorySecrets {
    async fn set(&self, service: &str, secret: &str) -> Result<(), String> {
        check_plain(secret)?;
        self.items
            .lock()
            .unwrap()
            .insert(service.into(), secret.into());
        Ok(())
    }

    async fn delete(&self, service: &str) -> Result<(), String> {
        self.items.lock().unwrap().remove(service);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_text_goes_into_a_keychain_command() {
        assert!(check_plain("bob_ak_123-abc.DEF").is_ok());
        for bad in ["", "a b", "a\"b", "a\\b", "a\nb"] {
            assert!(check_plain(bad).is_err(), "{bad:?}");
        }
    }
}
