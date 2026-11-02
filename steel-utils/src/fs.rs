//! Async filesystem calls for ephemeral-capable storage.
//!
//! wasm32 has no filesystem: worlds there are always ephemeral, so these fail
//! with [`io::ErrorKind::Unsupported`] and are never reached with a real path.

#[cfg(not(target_arch = "wasm32"))]
pub use tokio::fs::{create_dir_all, read_to_string, write};

#[cfg(target_arch = "wasm32")]
pub use unsupported::{create_dir_all, read_to_string, write};

#[cfg(target_arch = "wasm32")]
mod unsupported {
    use std::{io, path::Path};

    fn unsupported<T>() -> io::Result<T> {
        Err(io::ErrorKind::Unsupported.into())
    }

    pub async fn read_to_string(_: impl AsRef<Path>) -> io::Result<String> {
        unsupported()
    }

    pub async fn create_dir_all(_: impl AsRef<Path>) -> io::Result<()> {
        unsupported()
    }

    pub async fn write(_: impl AsRef<Path>, _: impl AsRef<[u8]>) -> io::Result<()> {
        unsupported()
    }
}
