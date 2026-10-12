//! Upstream's node20 → node24 migration decision for JavaScript actions.
//!
//! A port of actions/runner `cab9d1c` (v2.337.0): `NodeUtil.DetermineActionsNodeVersion`
//! and the node branches of `HandlerFactory.Create`. Server feature flags arrive
//! as job message variables; `FORCE_JAVASCRIPT_ACTIONS_TO_NODE24` and
//! `ACTIONS_ALLOW_USE_UNSECURE_NODE_VERSION` come from the step's workflow
//! environment or the runner process environment. Linux ARM32 phases are not
//! applicable: toolu ships no armv7 build.

use std::collections::{HashMap, HashSet};
use std::hash::BuildHasher;

use shared::{ActionStepDefinitionReference, VariableValue};

/// Workflow/system switch that opts node20 actions into node24.
pub const FORCE_NODE24_VARIABLE: &str = "FORCE_JAVASCRIPT_ACTIONS_TO_NODE24";
/// Workflow/system switch that keeps node20 actions on node20 once node24 is the default.
pub const ALLOW_UNSECURE_NODE_VARIABLE: &str = "ACTIONS_ALLOW_USE_UNSECURE_NODE_VERSION";
/// Upstream's Node 20 deprecation changelog link, quoted in every message.
pub const NODE20_DEPRECATION_URL: &str =
  "https://github.blog/changelog/2025-09-19-deprecation-of-node-20-on-github-actions-runners/";

const USE_NODE24_BY_DEFAULT_FLAG: &str = "actions.runner.usenode24bydefault";
const REQUIRE_NODE24_FLAG: &str = "actions.runner.requirenode24";
const WARN_ON_NODE20_FLAG: &str = "actions.runner.warnonnode20";
const NODE24_DEFAULT_DATE_VARIABLE: &str = "actions_runner_node24_default_date";
const NODE20_REMOVAL_DATE_VARIABLE: &str = "actions_runner_node20_removal_date";
const NODE24_DEFAULT_DATE: &str = "June 16th, 2026";
const NODE20_REMOVAL_DATE: &str = "September 23rd, 2026";

/// Server-controlled migration phase, read once per job from its variables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeMigrationFlags {
  /// Phase 2: node24 is the default for node20 actions.
  pub use_node24_by_default: bool,
  /// Phase 3: node20 actions always run on node24.
  pub require_node24: bool,
  /// Track node20 actions for the end-of-job deprecation warning.
  pub warn_on_node20: bool,
  /// Date quoted as "forced to run with Node.js 24 by default starting …".
  pub node24_default_date: String,
  /// Date quoted as "Node.js 20 will be removed from the runner on …".
  pub node20_removal_date: String,
}

impl Default for NodeMigrationFlags {
  fn default() -> Self {
    Self {
      use_node24_by_default: false,
      require_node24: false,
      warn_on_node20: false,
      node24_default_date: NODE24_DEFAULT_DATE.to_owned(),
      node20_removal_date: NODE20_REMOVAL_DATE.to_owned(),
    }
  }
}

impl NodeMigrationFlags {
  /// Read the flags the way upstream's `Variables` does: case-insensitive
  /// names, `bool.TryParse` values (anything unparseable is unset ⇒ `false`),
  /// and the hardcoded dates when a date variable is absent or empty.
  pub fn from_variables<S: BuildHasher>(variables: &HashMap<String, VariableValue, S>) -> Self {
    let date = |name: &str, fallback: &str| {
      variable(variables, name)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback)
        .to_owned()
    };
    Self {
      use_node24_by_default: try_parse_bool(variable(variables, USE_NODE24_BY_DEFAULT_FLAG)),
      require_node24: try_parse_bool(variable(variables, REQUIRE_NODE24_FLAG)),
      warn_on_node20: try_parse_bool(variable(variables, WARN_ON_NODE20_FLAG)),
      node24_default_date: date(NODE24_DEFAULT_DATE_VARIABLE, NODE24_DEFAULT_DATE),
      node20_removal_date: date(NODE20_REMOVAL_DATE_VARIABLE, NODE20_REMOVAL_DATE),
    }
  }
}

/// The runner process's values of the two switches (upstream's "system" source).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemNodeEnv {
  /// `FORCE_JAVASCRIPT_ACTIONS_TO_NODE24` in the runner process, if set.
  pub force_node24: Option<String>,
  /// `ACTIONS_ALLOW_USE_UNSECURE_NODE_VERSION` in the runner process, if set.
  pub allow_unsecure: Option<String>,
}

impl SystemNodeEnv {
  /// Read both switches from the runner process environment.
  pub fn from_process() -> Self {
    Self {
      force_node24: crate::config::var(FORCE_NODE24_VARIABLE),
      allow_unsecure: crate::config::var(ALLOW_UNSECURE_NODE_VARIABLE),
    }
  }
}

/// Why an action is listed in an end-of-job warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tracking {
  /// Declared node20, running on node24.
  UpgradedToNode24,
  /// Declared node20, still on node20 while `warnonnode20` is set.
  DeprecatedNode20,
}

/// The runtime a stage runs on, plus the messages upstream emits for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeSelection {
  /// Node major to run (20 or 24).
  pub major: u8,
  /// Step warning for a contradictory switch configuration.
  pub warning: Option<String>,
  /// Informational step output in phase 2.
  pub info: Option<String>,
  /// End-of-job listing this action belongs to, if any.
  pub tracking: Option<Tracking>,
}

/// Decide the Node major for an action declaring `node{declared}`.
///
/// node12/node16 run as node20 (upstream's redirect) and then follow the
/// node20 rules; node24 stays node24; any other major is returned unchanged.
pub fn select_node_version<S: BuildHasher>(
  declared: u8,
  flags: &NodeMigrationFlags,
  workflow_env: &HashMap<String, String, S>,
  system: &SystemNodeEnv,
) -> NodeSelection {
  let declared = if declared < 20 { 20 } else { declared };
  if declared != 20 {
    return NodeSelection {
      major: declared,
      warning: None,
      info: None,
      tracking: None,
    };
  }
  let (major, warning) = determine_actions_node_version(
    workflow_env,
    flags.use_node24_by_default,
    flags.require_node24,
    system,
  );
  let tracking = if major == 24 {
    Some(Tracking::UpgradedToNode24)
  } else {
    flags.warn_on_node20.then_some(Tracking::DeprecatedNode20)
  };
  let info = (flags.use_node24_by_default && !flags.require_node24 && major == 24).then(|| {
    format!(
      "Node 20 is being deprecated. This workflow is running with Node 24 by default. If you need \
       to temporarily use Node 20, you can set the {ALLOW_UNSECURE_NODE_VARIABLE}=true environment \
       variable. For more information see: {NODE20_DEPRECATION_URL}"
    )
  });
  NodeSelection {
    major,
    warning,
    info,
    tracking,
  }
}

/// Upstream `NodeUtil.DetermineActionsNodeVersion`: the node20 action's major
/// and the warning for both switches set true in the same source.
pub fn determine_actions_node_version<S: BuildHasher>(
  workflow_env: &HashMap<String, String, S>,
  use_node24_by_default: bool,
  require_node24: bool,
  system: &SystemNodeEnv,
) -> (u8, Option<String>) {
  if require_node24 {
    return (24, None);
  }
  let force = switch_details(
    FORCE_NODE24_VARIABLE,
    workflow_env,
    system.force_node24.as_deref(),
  );
  let allow = switch_details(
    ALLOW_UNSECURE_NODE_VARIABLE,
    workflow_env,
    system.allow_unsecure.as_deref(),
  );
  let both = force.is_true && allow.is_true;
  let both_from_workflow = both && force.from_workflow && allow.from_workflow;
  let both_from_system = both && force.from_system && allow.from_system;
  if both_from_workflow || both_from_system {
    let source = if both_from_workflow {
      "workflow"
    } else {
      "system"
    };
    let default = if use_node24_by_default { 24 } else { 20 };
    let warning = format!(
      "Both {FORCE_NODE24_VARIABLE} and {ALLOW_UNSECURE_NODE_VARIABLE} environment variables are \
       set to true in the {source} environment. This is likely a configuration error. Using the \
       default Node version: node{default}."
    );
    return (default, Some(warning));
  }
  if use_node24_by_default {
    return (if allow.is_true { 20 } else { 24 }, None);
  }
  (if force.is_true { 24 } else { 20 }, None)
}

/// Upstream `HandlerFactory.GetActionName`: `{name}{/path}@{ref}` for a
/// repository action, `None` for other references or an empty result.
pub fn action_name(reference: &ActionStepDefinitionReference) -> Option<String> {
  let is_repository = reference
    .ref_type
    .as_deref()
    .is_some_and(|kind| kind.eq_ignore_ascii_case("repository"));
  if !is_repository {
    return None;
  }
  let name = reference.name.as_deref().unwrap_or_default();
  let path = match reference.path.as_deref().unwrap_or_default() {
    "" => String::new(),
    path if name.is_empty() => path.to_owned(),
    path => format!("/{path}"),
  };
  let name = match reference.git_ref.as_deref().unwrap_or_default() {
    "" => format!("{name}{path}"),
    git_ref => format!("{name}{path}@{git_ref}"),
  };
  (!name.is_empty()).then_some(name)
}

/// Per-job migration state: the flags plus the actions each end-of-job
/// warning lists (deduplicated, as upstream's `HashSet`s).
#[derive(Debug, Default)]
pub struct NodeMigrationState {
  /// Flags read from the job message.
  pub flags: NodeMigrationFlags,
  upgraded: HashSet<String>,
  deprecated: HashSet<String>,
}

impl NodeMigrationState {
  /// Start a job's state from its message variables.
  pub fn for_job<S: BuildHasher>(variables: &HashMap<String, VariableValue, S>) -> Self {
    Self {
      flags: NodeMigrationFlags::from_variables(variables),
      ..Self::default()
    }
  }

  /// Record `action` under `tracking`.
  pub fn record(&mut self, tracking: Tracking, action: String) {
    match tracking {
      Tracking::UpgradedToNode24 => self.upgraded.insert(action),
      Tracking::DeprecatedNode20 => self.deprecated.insert(action),
    };
  }

  /// Upstream's "Complete job" warnings, deprecated before upgraded.
  pub fn completion_warnings(&self) -> Vec<String> {
    let mut warnings = Vec::new();
    if !self.deprecated.is_empty() {
      warnings.push(format!(
        "Node.js 20 actions are deprecated. The following actions are running on Node.js 20 and \
         may not work as expected: {}. Actions will be forced to run with Node.js 24 by default \
         starting {}. Node.js 20 will be removed from the runner on {}. Please check if updated \
         versions of these actions are available that support Node.js 24. To opt into Node.js 24 \
         now, set the {FORCE_NODE24_VARIABLE}=true environment variable on the runner or in your \
         workflow file. Once Node.js 24 becomes the default, you can temporarily opt out by \
         setting {ALLOW_UNSECURE_NODE_VARIABLE}=true. For more information see: \
         {NODE20_DEPRECATION_URL}",
        sorted_list(&self.deprecated),
        self.flags.node24_default_date,
        self.flags.node20_removal_date,
      ));
    }
    if !self.upgraded.is_empty() {
      warnings.push(format!(
        "Node.js 20 is deprecated. The following actions target Node.js 20 but are being forced to \
         run on Node.js 24: {}. For more information see: {NODE20_DEPRECATION_URL}",
        sorted_list(&self.upgraded),
      ));
    }
    warnings
  }
}

/// .NET `OrdinalIgnoreCase` ordering (upper-invariant), joined with `", "`.
fn sorted_list(actions: &HashSet<String>) -> String {
  let mut sorted: Vec<&String> = actions.iter().collect();
  sorted.sort_by(|a, b| {
    a.to_uppercase()
      .cmp(&b.to_uppercase())
      .then_with(|| a.cmp(b))
  });
  sorted
    .into_iter()
    .map(String::as_str)
    .collect::<Vec<_>>()
    .join(", ")
}

/// Where a switch is set and whether it reads true.
struct SwitchDetails {
  is_true: bool,
  from_workflow: bool,
  from_system: bool,
}

/// Upstream `GetEnvironmentVariableDetails`: a workflow value (even empty)
/// decides truth; the system value counts as a source only when non-empty.
fn switch_details<S: BuildHasher>(
  name: &str,
  workflow_env: &HashMap<String, String, S>,
  system_value: Option<&str>,
) -> SwitchDetails {
  let workflow = workflow_env.get(name).map(String::as_str);
  let from_system = system_value.is_some_and(|value| !value.is_empty());
  SwitchDetails {
    is_true: convert_to_boolean(workflow.or(system_value)),
    from_workflow: workflow.is_some(),
    from_system,
  }
}

/// Upstream `StringUtil.ConvertToBoolean` with a `false` default.
fn convert_to_boolean(value: Option<&str>) -> bool {
  matches!(
    value.map(str::to_lowercase).as_deref(),
    Some("1" | "true" | "$true")
  )
}

/// .NET `bool.TryParse` (trimmed, case-insensitive); unparseable ⇒ `false`.
fn try_parse_bool(value: Option<&str>) -> bool {
  value.is_some_and(|value| {
    value
      .trim_matches(|c: char| c.is_whitespace() || c == '\0')
      .eq_ignore_ascii_case("true")
  })
}

/// Case-insensitive variable lookup, preferring an exact-case key.
fn variable<'a, S: BuildHasher>(
  variables: &'a HashMap<String, VariableValue, S>,
  name: &str,
) -> Option<&'a str> {
  variables
    .get(name)
    .or_else(|| {
      variables
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value)
    })
    .map(|value| value.value.as_str())
}

#[cfg(test)]
#[path = "tests/migration.rs"]
mod tests;
