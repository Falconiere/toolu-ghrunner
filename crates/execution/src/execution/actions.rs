/// Downloads and extracts action tarballs, and tracks the local action cache.
pub mod downloader;
/// `action.yml` / `action.yaml` manifest parsing.
pub mod manifest;
/// Single-flight job-start action prefetch (`ActionFetcher`, `spawn_prefetch`).
pub mod prefetch;
/// Parses `uses:` references and resolves them to a downloadable action.
pub mod resolver;
