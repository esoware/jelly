//! Endpoint overrides and health.
//!
//! The engine stores them, but the registry the pools read lives in the network layer, so
//! after an edit the caller reloads it and then calls [`WalletEngine::endpoints_changed`].
//!
//! A URL must already be canonical, because the database keys on the string it is given.

use jelly_core::{ChainId, CoreError, EndpointHealthRecord, EndpointOverride};

use crate::WalletEngine;

impl WalletEngine {
    pub async fn list_endpoints(&self) -> Result<Vec<(ChainId, EndpointOverride)>, CoreError> {
        Ok(self.inner.store.list_endpoints().await?)
    }

    pub async fn add_endpoint(
        &self,
        chain: ChainId,
        url: String,
        display: String,
    ) -> Result<(), CoreError> {
        Ok(self.inner.store.add_endpoint(chain, url, display).await?)
    }

    pub async fn set_endpoint_enabled(
        &self,
        chain: ChainId,
        url: String,
        is_enabled: bool,
    ) -> Result<(), CoreError> {
        Ok(self.inner.store.set_endpoint_enabled(chain, url, is_enabled).await?)
    }

    pub async fn set_endpoint_pinned(
        &self,
        chain: ChainId,
        url: String,
        is_pinned: bool,
    ) -> Result<(), CoreError> {
        Ok(self.inner.store.set_endpoint_pinned(chain, url, is_pinned).await?)
    }

    pub async fn delete_endpoint(&self, chain: ChainId, url: String) -> Result<(), CoreError> {
        Ok(self.inner.store.delete_endpoint(chain, url).await?)
    }

    pub async fn list_endpoint_health(&self) -> Result<Vec<EndpointHealthRecord>, CoreError> {
        Ok(self.inner.store.list_endpoint_health().await?)
    }

    /// Writes every record in one transaction. This runs on exit, and with
    /// `synchronous = FULL` a transaction per row would cost an fsync each.
    pub async fn put_endpoint_health(
        &self,
        records: Vec<EndpointHealthRecord>,
    ) -> Result<(), CoreError> {
        if records.is_empty() {
            return Ok(());
        }
        self.inner
            .store
            .transact(move |writer| {
                for record in &records {
                    writer.put_endpoint_health(record)?;
                }
                Ok(())
            })
            .await?;
        Ok(())
    }
}
