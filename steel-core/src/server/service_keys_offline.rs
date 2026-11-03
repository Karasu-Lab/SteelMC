//! wasm32 stand-in for the Minecraft services key fetch. Online mode cannot run
//! there, so no profile-key validator is ever available.

use std::sync::Arc;

use steel_crypto::signature::ProfileKeyValidator;
use thiserror::Error;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Error)]
pub(super) enum ServiceKeyError {}

/// Holds no keys; see the module docs.
pub(super) struct ServiceKeyStore;

impl ServiceKeyStore {
    pub(super) fn new(_endpoint: Option<&str>) -> Result<Self, ServiceKeyError> {
        Ok(Self)
    }

    pub(super) fn profile_key_validator(&self) -> Option<Arc<ProfileKeyValidator>> {
        None
    }

    /// Reports readiness immediately: there is nothing to fetch.
    pub(super) fn start(
        self: &Arc<Self>,
        _cancel_token: CancellationToken,
    ) -> oneshot::Receiver<()> {
        let (ready_tx, ready_rx) = oneshot::channel();
        let _ = ready_tx.send(());
        ready_rx
    }
}
