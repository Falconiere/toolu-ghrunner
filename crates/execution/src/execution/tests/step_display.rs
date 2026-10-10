//! Display names generated from the issue-99 capture (`step-attrs-99.yml`,
//! run 37992025403, toolu lane). Expected names are the reference lane's
//! jobs-API step names from the same run, with its matrix lane swapped in.

use super::*;
use shared::AgentJobRequestMessage;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const ATTRS: &str = include_str!("../../../tests/step_attrs_99_attrs.json");
const BAD_PREFIX: &str = "Encountered an error when evaluating display name ${{ format('Bad {0}', \
                          fromJSON(steps.prior.outputs.bad)) }}. The template is not valid. \
                          .github/workflows/step-attrs-99.yml (Line: 154, Col: 15): ";
const NOPE_PREFIX: &str = "Encountered an error when evaluating display name ${{ fromJSON('nope') \
                           }}. The template is not valid. .github/workflows/step-attrs-99.yml \
                           (Line: 157, Col: 15): ";

/// The captured message and a context holding its contexts and file table.
fn captured() -> Result<(AgentJobRequestMessage, ExecutionContext), serde_json::Error> {
  let msg: AgentJobRequestMessage = serde_json::from_str(ATTRS)?;
  let mut ctx = ExecutionContext::new_for_test();
  ctx.import_contexts(&msg.context_data);
  ctx.set_file_table(msg.file_table.clone());
  Ok((msg, ctx))
}

fn step<'a>(msg: &'a AgentJobRequestMessage, name: &str) -> Result<&'a ActionStep, String> {
  msg
    .steps
    .iter()
    .find(|step| step.context_name.as_deref() == Some(name))
    .ok_or_else(|| format!("captured step {name} missing"))
}

/// Run the captured `prior` step's outputs into the context.
fn run_prior(ctx: &mut ExecutionContext) {
  for (key, value) in [
    ("flag", "true"),
    ("minutes", "1"),
    ("label", "from-prior"),
    ("bad", "not-json"),
  ] {
    ctx.set_step_output("prior", key, value);
  }
}

/// The names reported for each step and every warning, in order.
async fn job_start(
  msg: &AgentJobRequestMessage,
  ctx: &mut ExecutionContext,
) -> Result<Vec<(String, String)>, String> {
  let (tx, mut rx) = mpsc::channel(64);
  name_at_job_start(&msg.steps, ctx, &tx).await;
  drop(tx);
  let mut warnings = Vec::new();
  while let Some(event) = rx.recv().await {
    let RunnerEvent::Log { step_id, line, .. } = event else {
      return Err(format!("unexpected event {event:?}"));
    };
    warnings.push((step_id, line));
  }
  Ok(warnings)
}

#[test]
fn unnamed_steps_are_named_after_their_source() -> TestResult {
  let (msg, ctx) = captured()?;
  let mut checkout = step(&msg, "__actions_checkout")?.clone();
  assert_eq!(
    display_for(&checkout, &ctx).current,
    "Run actions/checkout@v4"
  );
  checkout.reference.git_ref = None;
  assert_eq!(display_for(&checkout, &ctx).current, "Run actions/checkout");

  let mut local = step(&msg, "composite_budget")?.clone();
  local.display_name_token = None;
  assert_eq!(
    display_for(&local, &ctx).current,
    "Run ./.github/actions/step-attrs-99-composite"
  );

  let mut remote = step(&msg, "node_stages")?.clone();
  remote.display_name_token = None;
  assert_eq!(
    display_for(&remote, &ctx).current,
    "Run Falconiere/toolu-ghrunner/.github/actions/step-attrs-99-node@\
     7ac4fa2e45c8c4725a9ad44db4eb6b9682064260"
  );

  let mut prior = step(&msg, "prior")?.clone();
  prior.display_name_token = None;
  assert_eq!(display_for(&prior, &ctx).current, "Run {");
  Ok(())
}

#[tokio::test]
async fn job_start_evaluates_message_contexts_and_defers_step_contexts() -> TestResult {
  let (msg, mut ctx) = captured()?;
  let warnings = job_start(&msg, &mut ctx).await?;

  let names = |name: &str| -> Result<String, String> {
    let id = &step(&msg, name)?.id;
    ctx
      .step_display(id)
      .map(|display| display.current.clone())
      .ok_or_else(|| format!("{name} was not named"))
  };
  assert_eq!(names("prior")?, "Prior outputs");
  assert_eq!(names("matrix_attrs")?, "Matrix toolu-linux input ");
  assert_eq!(names("__run_9")?, "Run echo lane toolu-linux");
  assert_eq!(
    names("deferred_coe")?,
    "Deferred ${{ steps.prior.outputs.label }}"
  );
  assert_eq!(
    names("__run_10")?,
    "Run echo prior ${{ steps.prior.outputs.label }}"
  );
  assert_eq!(
    names("__run_7")?,
    "Bad ${{ fromJSON(steps.prior.outputs.bad) }}"
  );
  assert_eq!(names("__run_8")?, FALLBACK_NAME);

  assert_eq!(warnings.len(), 1, "{warnings:?}");
  let (step_id, line) = warnings.first().ok_or("no warning")?;
  assert_eq!(step_id, SETUP_STEP_ID);
  assert!(
    line.starts_with(&format!("##[warning]{NOPE_PREFIX}")),
    "{line}"
  );
  Ok(())
}

#[tokio::test]
async fn main_stage_evaluates_live_contexts_and_keeps_names_on_failure() -> TestResult {
  let (msg, mut ctx) = captured()?;
  job_start(&msg, &mut ctx).await?;
  run_prior(&mut ctx);

  let deferred = name_at_main(step(&msg, "deferred_coe")?, &mut ctx);
  assert_eq!(deferred, ("Deferred from-prior".to_owned(), None));
  let script = name_at_main(step(&msg, "__run_10")?, &mut ctx);
  assert_eq!(script, ("Run echo prior from-prior".to_owned(), None));
  let matrix = name_at_main(step(&msg, "matrix_attrs")?, &mut ctx);
  assert_eq!(matrix, ("Matrix toolu-linux input ".to_owned(), None));

  let (bad, warning) = name_at_main(step(&msg, "__run_7")?, &mut ctx);
  assert_eq!(bad, "Bad ${{ fromJSON(steps.prior.outputs.bad) }}");
  let warning = warning.ok_or("bad name logged no warning")?;
  assert!(warning.starts_with(BAD_PREFIX), "{warning}");

  let (nope, warning) = name_at_main(step(&msg, "__run_8")?, &mut ctx);
  assert_eq!(nope, FALLBACK_NAME);
  let warning = warning.ok_or("nope name logged no warning")?;
  assert!(warning.starts_with(NOPE_PREFIX), "{warning}");

  let initial = ctx
    .step_display(&step(&msg, "deferred_coe")?.id)
    .map(|display| display.initial.clone());
  assert_eq!(
    initial.as_deref(),
    Some("Deferred ${{ steps.prior.outputs.label }}")
  );
  Ok(())
}

#[tokio::test]
async fn evaluated_names_are_masked() -> TestResult {
  let (msg, mut ctx) = captured()?;
  job_start(&msg, &mut ctx).await?;
  run_prior(&mut ctx);
  ctx
    .masker()
    .lock()
    .map_err(|error| error.to_string())?
    .add_secret("from-prior");
  let (name, warning) = name_at_main(step(&msg, "deferred_coe")?, &mut ctx);
  assert_eq!(name, "Deferred ***");
  assert_eq!(warning, None);
  Ok(())
}

#[test]
fn only_the_first_line_after_leading_whitespace_is_kept() {
  assert_eq!(
    format_step_name(RUN_PREFIX, "\n  echo a\necho b"),
    "Run echo a"
  );
  assert_eq!(format_step_name(RUN_PREFIX, ""), "");
}
