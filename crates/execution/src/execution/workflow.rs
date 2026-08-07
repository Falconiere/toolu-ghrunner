/// Builds the job dependency graph (`needs:`) and its execution plan.
pub mod job_graph;
/// Expands a job's `strategy.matrix` into concrete job instances.
pub mod matrix;
/// Job graph orchestration: which jobs are ready, running, or done.
pub mod orchestrator;
/// Workflow YAML parser.
pub mod parser;
/// Reusable workflow resolution, validation, and invocation.
pub mod reusable;
/// Workflow `on:` trigger parsing and matching.
pub mod trigger;
/// Parsed workflow YAML types (jobs, steps, defaults, etc.).
pub mod types;
