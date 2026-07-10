//! Workflow YAML parser.

mod jobs;
mod parse;
mod raw_types;
mod triggers;

pub use parse::parse_workflow;
