//! Job message types from the GitHub Actions protocol.
//!
//! # Public API
//!
//! - [`AgentJobRequestMessage`] -- full job request message
//! - [`TaskOrchestrationPlanReference`] -- plan reference in the job
//! - [`ActionStep`] -- a single step in the job
//! - [`ActionStepDefinitionReference`] -- reference to the action/script definition
//! - [`VariableValue`] -- a variable in the job message
//! - [`MaskHint`] -- hint for values to mask in logs
//! - [`JobResources`] -- resources available to the job
//! - [`JobEndpoint`] -- an endpoint in the job resources
//! - [`JobAuthorization`] -- authorization data for an endpoint
//! - [`WorkspaceOptions`] -- workspace options from the job message
//! - [`PipelineContextData`] -- context data from the pipeline
//! - [`DictEntry`] -- key-value pair in dictionary context data
//! - [`TemplateToken`] -- template token from the job message
mod context_data;
mod context_data_de;
mod request;
mod resource;
mod step;
mod template_token;

pub use context_data::{DictEntry, PipelineContextData};
pub use request::{AgentJobRequestMessage, TaskOrchestrationPlanReference};
pub use resource::{
  JobAuthorization, JobEndpoint, JobResources, MaskHint, VariableValue, WorkspaceOptions,
};
pub use step::{ActionStep, ActionStepDefinitionReference};
pub use template_token::TemplateToken;
