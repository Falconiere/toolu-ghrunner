//! Local HTTP cache service mimicking GitHub's cache API.

mod handlers;
mod lifecycle;

pub use lifecycle::CacheService;
