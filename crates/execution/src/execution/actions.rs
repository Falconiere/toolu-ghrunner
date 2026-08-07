/// Downloads and extracts action tarballs, and tracks the local action cache.
pub mod downloader;
/// `action.yml` / `action.yaml` manifest parsing.
pub mod manifest;
/// Parses `uses:` references and resolves them to a downloadable action.
pub mod resolver;
