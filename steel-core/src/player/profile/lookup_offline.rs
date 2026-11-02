//! wasm32 stand-in for online profile lookup: there is no identity service to
//! reach, so online-name lookups always fail.

use thiserror::Error;

use super::known_players::KnownPlayer;

/// Failure while resolving a player identity through the configured profile service.
#[derive(Debug, Error)]
pub enum ProfileLookupError {
    /// No profile exists for the requested name.
    #[error("Unknown player {0}")]
    UnknownPlayer(String),
    /// Online lookups cannot run on this platform.
    #[error("Online profile lookup is unavailable on this platform")]
    Unavailable,
}

/// Placeholder for the HTTP client native builds use for profile lookups.
#[derive(Clone, Debug, Default)]
pub struct ProfileLookupClient;

impl ProfileLookupClient {
    /// Creates the placeholder client.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// Always fails: see [`ProfileLookupError::Unavailable`].
#[expect(clippy::unused_async, reason = "mirrors the native async signature")]
pub async fn lookup_online_profile(
    _client: &ProfileLookupClient,
    _profile_server: Option<&str>,
    _name: &str,
) -> Result<KnownPlayer, ProfileLookupError> {
    Err(ProfileLookupError::Unavailable)
}
