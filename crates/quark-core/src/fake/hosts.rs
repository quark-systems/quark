use std::sync::Mutex;

use async_trait::async_trait;

use crate::host::{Health, Host, HostRegistry};
use crate::{CoreError, HostId, Result};

/// A [`HostRegistry`] in memory. Registering an existing id replaces it.
#[derive(Debug, Default)]
pub struct MemoryHosts {
    hosts: Mutex<Vec<Host>>,
}

#[async_trait]
impl HostRegistry for MemoryHosts {
    async fn hosts(&self) -> Result<Vec<Host>> {
        Ok(self.hosts.lock().unwrap().clone())
    }

    async fn register(&self, host: Host) -> Result<()> {
        let mut hosts = self.hosts.lock().unwrap();
        hosts.retain(|h| h.id != host.id);
        hosts.push(host);
        Ok(())
    }

    async fn set_health(&self, id: &HostId, health: Health) -> Result<()> {
        let mut hosts = self.hosts.lock().unwrap();
        let h = hosts
            .iter_mut()
            .find(|h| &h.id == id)
            .ok_or_else(|| CoreError::NotFound(format!("host {id}")))?;
        h.health = health;
        Ok(())
    }
}
