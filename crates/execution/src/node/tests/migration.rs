//! Tests for `migration`: upstream's own `NodeUtilL0` oracle rows (committed
//! verbatim with provenance), flags parsed from a captured github.com job
//! message, .NET parsing boundaries, upstream's message texts, action names
//! from captured step references, and the end-of-job warnings.

use super::*;
use shared::AgentJobRequestMessage;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const L0_ROWS: &str = include_str!("../../../tests/node_migration_upstream_l0.json");
const CAPTURED_FLAGS: &str = include_str!("../../../tests/job_outputs_message.json");
const CAPTURED_REFERENCES: &str = include_str!("../../../tests/step_attrs_99_attrs.json");

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
  pairs
    .iter()
    .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
    .collect()
}

fn vars(pairs: &[(&str, &str)]) -> HashMap<String, VariableValue> {
  pairs
    .iter()
    .map(|(key, value)| {
      (
        (*key).to_owned(),
        VariableValue {
          value: (*value).to_owned(),
          is_secret: false,
        },
      )
    })
    .collect()
}

fn system(force: Option<&str>, allow: Option<&str>) -> SystemNodeEnv {
  SystemNodeEnv {
    force_node24: force.map(str::to_owned),
    allow_unsecure: allow.map(str::to_owned),
  }
}

fn flag(row: &serde_json::Value, name: &str) -> Result<bool, Box<dyn std::error::Error>> {
  row
    .get(name)
    .and_then(serde_json::Value::as_bool)
    .ok_or_else(|| format!("row field {name} missing").into())
}

#[test]
fn upstream_node_util_l0_rows_select_the_same_version_and_warning() -> TestResult {
  let fixture: serde_json::Value = serde_json::from_str(L0_ROWS)?;
  let rows = fixture
    .get("rows")
    .and_then(serde_json::Value::as_array)
    .ok_or("rows missing")?;
  assert_eq!(rows.len(), 27, "NodeUtilL0 has 12 + 15 InlineData rows");
  for row in rows {
    let truthy = |on: bool| on.then_some("true");
    let mut workflow = Vec::new();
    if flag(row, "workflow_force_node24")? {
      workflow.push((FORCE_NODE24_VARIABLE, "true"));
    }
    if flag(row, "workflow_allow_unsecure")? {
      workflow.push((ALLOW_UNSECURE_NODE_VARIABLE, "true"));
    }
    let (major, warning) = determine_actions_node_version(
      &env(&workflow),
      flag(row, "use_node24_by_default")?,
      flag(row, "require_node24")?,
      &system(
        truthy(flag(row, "system_force_node24")?),
        truthy(flag(row, "system_allow_unsecure")?),
      ),
    );
    let line = row.get("line").ok_or("line missing")?;
    assert_eq!(
      Some(format!("node{major}").as_str()),
      row.get("expected").and_then(serde_json::Value::as_str),
      "NodeUtilL0.cs line {line}"
    );
    assert_eq!(
      warning.is_some(),
      flag(row, "expect_warning")?,
      "NodeUtilL0.cs line {line}"
    );
    if let Some(warning) = warning {
      assert!(warning.contains("Both") && warning.contains("are set to true"));
    }
  }
  Ok(())
}

#[test]
fn captured_github_message_flags_parse_as_phase_three() -> TestResult {
  let message: AgentJobRequestMessage = serde_json::from_str(CAPTURED_FLAGS)?;
  let flags = NodeMigrationFlags::from_variables(&message.variables);
  assert_eq!(
    flags,
    NodeMigrationFlags {
      use_node24_by_default: true,
      require_node24: true,
      warn_on_node20: true,
      node24_default_date: "June 16th, 2026".to_owned(),
      node20_removal_date: "September 23rd, 2026".to_owned(),
    }
  );
  Ok(())
}

#[test]
fn flags_follow_dotnet_bool_try_parse_and_ignore_key_case() {
  let flags = NodeMigrationFlags::from_variables(&vars(&[
    ("Actions.Runner.UseNode24ByDefault", " TRUE "),
    ("actions.runner.requirenode24", "1"),
    ("actions.runner.warnonnode20", "yes"),
    ("actions_runner_node24_default_date", ""),
    ("ACTIONS_RUNNER_NODE20_REMOVAL_DATE", "October 1st, 2026"),
  ]));
  assert!(flags.use_node24_by_default, "TryParse trims and ignores case");
  assert!(!flags.require_node24, "TryParse rejects \"1\"");
  assert!(!flags.warn_on_node20, "TryParse rejects \"yes\"");
  assert_eq!(flags.node24_default_date, "June 16th, 2026");
  assert_eq!(flags.node20_removal_date, "October 1st, 2026");
  assert_eq!(
    NodeMigrationFlags::from_variables(&HashMap::new()),
    NodeMigrationFlags::default()
  );
}

#[test]
fn switches_follow_string_util_convert_to_boolean() {
  let phase1 = NodeMigrationFlags::default();
  let none = SystemNodeEnv::default();
  for value in ["1", "true", "TRUE", "$true", "$TRUE"] {
    let selected = select_node_version(20, &phase1, &env(&[(FORCE_NODE24_VARIABLE, value)]), &none);
    assert_eq!(selected.major, 24, "{value:?} is true");
  }
  for value in ["", "yes", "0", "false", " true"] {
    let selected = select_node_version(20, &phase1, &env(&[(FORCE_NODE24_VARIABLE, value)]), &none);
    assert_eq!(selected.major, 20, "{value:?} is not true");
  }
  // A workflow value, even empty, decides truth over the runner process.
  let selected = select_node_version(
    20,
    &phase1,
    &env(&[(FORCE_NODE24_VARIABLE, "")]),
    &system(Some("true"), None),
  );
  assert_eq!(selected.major, 20);
}

#[test]
fn both_switches_from_different_sources_are_not_a_conflict() {
  let selected = select_node_version(
    20,
    &NodeMigrationFlags::default(),
    &env(&[(FORCE_NODE24_VARIABLE, "true")]),
    &system(None, Some("true")),
  );
  assert_eq!((selected.major, selected.warning), (24, None));
}

#[test]
fn same_source_conflict_uses_upstream_warning_text() {
  let flags = NodeMigrationFlags {
    use_node24_by_default: true,
    ..NodeMigrationFlags::default()
  };
  let selected = select_node_version(
    20,
    &flags,
    &env(&[
      (FORCE_NODE24_VARIABLE, "true"),
      (ALLOW_UNSECURE_NODE_VARIABLE, "true"),
    ]),
    &SystemNodeEnv::default(),
  );
  assert_eq!(selected.major, 24);
  assert_eq!(
    selected.warning.as_deref(),
    Some(
      "Both FORCE_JAVASCRIPT_ACTIONS_TO_NODE24 and ACTIONS_ALLOW_USE_UNSECURE_NODE_VERSION \
       environment variables are set to true in the workflow environment. This is likely a \
       configuration error. Using the default Node version: node24."
    )
  );
  let system_conflict = select_node_version(
    20,
    &NodeMigrationFlags::default(),
    &HashMap::new(),
    &system(Some("1"), Some("$true")),
  );
  assert_eq!(system_conflict.major, 20);
  assert!(
    system_conflict
      .warning
      .is_some_and(|warning| warning.contains("in the system environment"))
  );
}

#[test]
fn phase_two_default_prints_upstream_info_line() {
  let flags = NodeMigrationFlags {
    use_node24_by_default: true,
    ..NodeMigrationFlags::default()
  };
  let selected = select_node_version(20, &flags, &HashMap::new(), &SystemNodeEnv::default());
  assert_eq!(selected.major, 24);
  assert_eq!(
    selected.info.as_deref(),
    Some(
      "Node 20 is being deprecated. This workflow is running with Node 24 by default. If you need \
       to temporarily use Node 20, you can set the ACTIONS_ALLOW_USE_UNSECURE_NODE_VERSION=true \
       environment variable. For more information see: \
       https://github.blog/changelog/2025-09-19-deprecation-of-node-20-on-github-actions-runners/"
    )
  );
  let required = NodeMigrationFlags {
    require_node24: true,
    ..flags
  };
  let phase3 = select_node_version(20, &required, &HashMap::new(), &SystemNodeEnv::default());
  assert_eq!((phase3.major, phase3.info), (24, None), "phase 3 is silent");
}

#[test]
fn declared_versions_redirect_and_track_like_upstream() {
  let required = NodeMigrationFlags {
    require_node24: true,
    ..NodeMigrationFlags::default()
  };
  let warn = NodeMigrationFlags {
    warn_on_node20: true,
    ..NodeMigrationFlags::default()
  };
  let none = SystemNodeEnv::default();
  let no_env = HashMap::new();
  for declared in [12, 16, 20] {
    let upgraded = select_node_version(declared, &required, &no_env, &none);
    assert_eq!(
      (upgraded.major, upgraded.tracking),
      (24, Some(Tracking::UpgradedToNode24))
    );
    let deprecated = select_node_version(declared, &warn, &no_env, &none);
    assert_eq!(
      (deprecated.major, deprecated.tracking),
      (20, Some(Tracking::DeprecatedNode20))
    );
    let quiet = select_node_version(declared, &NodeMigrationFlags::default(), &no_env, &none);
    assert_eq!((quiet.major, quiet.tracking), (20, None));
  }
  let node24 = select_node_version(24, &warn, &env(&[(ALLOW_UNSECURE_NODE_VARIABLE, "true")]), &none);
  assert_eq!(
    node24,
    NodeSelection {
      major: 24,
      warning: None,
      info: None,
      tracking: None,
    }
  );
}

#[test]
fn action_names_match_upstream_for_captured_references() -> TestResult {
  let message: AgentJobRequestMessage = serde_json::from_str(CAPTURED_REFERENCES)?;
  let names: Vec<Option<String>> = message
    .steps
    .iter()
    .map(|step| action_name(&step.reference))
    .collect();
  assert!(names.contains(&Some("actions/checkout@v4".to_owned())));
  assert!(names.contains(&Some(
    "Falconiere/toolu-ghrunner/.github/actions/step-attrs-99-node@\
     7ac4fa2e45c8c4725a9ad44db4eb6b9682064260"
      .to_owned()
  )));
  assert!(names.contains(&Some(
    "./.github/actions/step-attrs-99-composite".to_owned()
  )));
  assert!(names.contains(&None), "script steps have no action name");
  Ok(())
}

#[test]
fn completion_warnings_dedupe_sort_and_quote_dates() {
  // .NET OrdinalIgnoreCase compares upper-cased text, so `X` (0x58) sorts
  // before `_` (0x5F): `betax` precedes `beta_x`.
  let mut state = NodeMigrationState::for_job(&vars(&[(
    "actions_runner_node20_removal_date",
    "October 1st, 2026",
  )]));
  for action in ["zeta/act@v1", "Alpha/act@v1", "beta_x/act@v1", "betax/act@v1", "zeta/act@v1"] {
    state.record(Tracking::UpgradedToNode24, action.to_owned());
  }
  state.record(Tracking::DeprecatedNode20, "./local".to_owned());
  let warnings = state.completion_warnings();
  assert_eq!(warnings.len(), 2);
  assert_eq!(
    warnings.first().map(String::as_str),
    Some(
      "Node.js 20 actions are deprecated. The following actions are running on Node.js 20 and \
       may not work as expected: ./local. Actions will be forced to run with Node.js 24 by \
       default starting June 16th, 2026. Node.js 20 will be removed from the runner on October \
       1st, 2026. Please check if updated versions of these actions are available that support \
       Node.js 24. To opt into Node.js 24 now, set the FORCE_JAVASCRIPT_ACTIONS_TO_NODE24=true \
       environment variable on the runner or in your workflow file. Once Node.js 24 becomes the \
       default, you can temporarily opt out by setting \
       ACTIONS_ALLOW_USE_UNSECURE_NODE_VERSION=true. For more information see: \
       https://github.blog/changelog/2025-09-19-deprecation-of-node-20-on-github-actions-runners/"
    )
  );
  assert_eq!(
    warnings.get(1).map(String::as_str),
    Some(
      "Node.js 20 is deprecated. The following actions target Node.js 20 but are being forced to \
       run on Node.js 24: Alpha/act@v1, betax/act@v1, beta_x/act@v1, zeta/act@v1. For more information see: \
       https://github.blog/changelog/2025-09-19-deprecation-of-node-20-on-github-actions-runners/"
    )
  );
  assert!(NodeMigrationState::default().completion_warnings().is_empty());
}
