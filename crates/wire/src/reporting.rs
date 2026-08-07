/// V1-vs-V2 protocol detection (GHES vs github.com).
pub mod feature_detection;
/// Live log streaming via WebSocket to the GitHub Actions UI.
pub mod live_log;
/// Decision of which Azure blob upload mode (`BlockBlob` vs `AppendBlob`) to use.
pub mod log_upload;
/// Results Service Twirp request/response shapes and signed-URL helpers.
pub mod results_service;
mod results_types;
/// Run Service request/response shapes for job acquire/renew/complete.
pub mod run_service;
mod types;

pub use types::{Annotation, Conclusion as ReportConclusion, Status, StepResult};
