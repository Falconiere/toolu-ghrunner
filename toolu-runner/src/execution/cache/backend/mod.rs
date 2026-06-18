//! Cache backend storage abstraction and filesystem implementation.

pub mod layered;
mod local_disk;
pub mod remote;
mod traits;

pub use layered::LayeredBackend;
pub use local_disk::LocalDiskBackend;
pub use remote::{RemoteBackend, RemoteConfig};
pub use traits::{CacheBackend, CacheEntry};
