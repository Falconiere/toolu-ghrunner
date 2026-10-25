//! Scope and details staging: the runner group or repository picker,
//! the runner name, the same-name collision check, and the linger
//! answer.

use super::apply::AuthContext;
use super::args::SetupArgs;
use super::github;
use super::machine::MachineFacts;
use super::plan::{SetupError, Target};
use super::prompts::Prompts;
use super::render::Out;

/// The shared scope-staging inputs (one credential, one account).
pub(crate) struct ScopeFlow<'a> {
  /// GitHub web base.
  pub(crate) github_base: &'a str,
  /// The runner home (collision resume check).
  pub(crate) home: &'a std::path::Path,
  /// The caller's flags.
  pub(crate) args: &'a SetupArgs,
  /// The step renderer.
  pub(crate) out: Out,
  /// The HTTP client for live reads.
  pub(crate) client: &'a reqwest::Client,
}

/// Scope: the runner group for an org, or the repositories to register.
pub(crate) async fn resolve_scope(
  flow: &ScopeFlow<'_>,
  target: &mut Target,
  auth_ctx: &AuthContext,
  prompts: &mut Option<Prompts>,
) -> Result<(Option<String>, Vec<String>), SetupError> {
  let (github_base, args, out, client) = (flow.github_base, flow.args, flow.out, flow.client);
  match target {
    Target::Org { org } => {
      let url = format!("{github_base}/{org}");
      let base = github::api_base(&url)?;
      if let Some(group) = &args.runner_group {
        let resolved =
          crate::register_auth::resolve_runner_group(group, &url, &auth_ctx.bearer, client)
            .await
            .map_err(|e| SetupError::usage(e.to_string()))?;
        return Ok((Some(resolved), Vec::new()));
      }
      let groups = github::list_groups(client, &base, org, &auth_ctx.bearer).await;
      let Some(groups) = groups else {
        out.warn(
          "runner-group",
          "could not list runner groups; using Default (name another with --runner-group)",
        );
        return Ok((Some("Default".to_owned()), Vec::new()));
      };
      if groups.len() <= 1 {
        if let Some(group) = groups.first() {
          out.done("runner-group", &format!("{} (the only group)", group.name));
          if group.default || group.id == 1 {
            out.warn(
              "runner-group",
              "the Default group doesn't serve public repos",
            );
          }
          return Ok((Some(group.id.to_string()), Vec::new()));
        }
        return Ok((Some("Default".to_owned()), Vec::new()));
      }
      let chosen = if let Some(prompts) = prompts.as_mut() {
        let names: Vec<String> = groups.iter().map(|g| g.name.clone()).collect();
        let default = groups.iter().position(|g| g.default).unwrap_or(0);
        let index = prompts
          .select("Runner group", &names, default)
          .map_err(SetupError::from)?;
        groups.get(index).map_or(1, |group| group.id)
      } else {
        groups.iter().find(|g| g.default).map_or(1, |g| g.id)
      };
      if github::is_default_group(&groups, chosen) {
        out.warn(
          "runner-group",
          "the Default group doesn't serve public repos",
        );
      }
      Ok((Some(chosen.to_string()), Vec::new()))
    },
    Target::Repos { repos } => {
      if !repos.is_empty() {
        return Ok((None, repos.clone()));
      }
      let api = github::api_base(&format!("{github_base}/x"))?;
      let available =
        github::list_repositories(client, &api, &auth_ctx.bearer, auth_ctx.app_installation)
          .await?;
      if available.is_empty() {
        return Err(SetupError::usage(
          "the credential can see no repositories; pass --repo <owner/name> for one it administers",
        ));
      }
      let Some(prompts) = prompts.as_mut() else {
        return Err(SetupError::usage(
          "no repositories chosen: pass --repo <owner/name> (repeatable)",
        ));
      };
      let picked = prompts
        .multi_select(
          "Which repositories?",
          &available,
          &vec![false; available.len()],
        )
        .map_err(SetupError::from)?;
      if picked.is_empty() {
        return Err(SetupError::usage("no repositories chosen"));
      }
      let chosen: Vec<String> = picked
        .iter()
        .filter_map(|i| available.get(*i).cloned())
        .collect();
      // One owner per plan (one credential/installation).
      let owner = chosen
        .first()
        .and_then(|repo| repo.split('/').next())
        .unwrap_or_default()
        .to_owned();
      if chosen
        .iter()
        .any(|r| r.split('/').next() != Some(owner.as_str()))
      {
        return Err(SetupError::usage(
          "the picker chose repositories from several owners; one setup run covers one account",
        ));
      }
      repos.clone_from(&chosen);
      Ok((None, chosen))
    },
  }
}

/// The runner name: flag, prompt, or the hostname.
pub(crate) fn resolve_name(
  args: &SetupArgs,
  prompts: &mut Option<Prompts>,
) -> Result<String, SetupError> {
  if let Some(name) = &args.name {
    return Ok(name.clone());
  }
  let default = crate::cli::runner_name_or_hostname(None);
  match prompts.as_mut() {
    Some(prompts) => prompts
      .input("Runner name", &default)
      .map_err(SetupError::from),
    None => Ok(default),
  }
}

/// Collision: a same-name runner already in the target needs
/// `--replace` (unattended) or a confirm (interactive).
pub(crate) async fn check_collision(
  flow: &ScopeFlow<'_>,
  target: &Target,
  name: &str,
  auth_ctx: &AuthContext,
  prompts: &mut Option<Prompts>,
) -> Result<(), SetupError> {
  if flow.args.mode.replace {
    return Ok(());
  }
  for url in target.urls(flow.github_base) {
    // A locally registered target is a resume, not a collision: the
    // runner on GitHub is this registration's own.
    if crate::register_cmd::register_config_path(&url, flow.home).is_ok_and(|path| path.exists()) {
      continue;
    }
    if let Ok(Some(_existing)) =
      github::find_runner(flow.client, &url, &auth_ctx.bearer, name).await
    {
      flow
        .out
        .warn("runner", &format!("'{name}' is already a runner in {url}"));
      match prompts.as_mut() {
        Some(prompts) => {
          let yes = prompts
            .confirm("Replace it?", false)
            .map_err(SetupError::from)?;
          if !yes {
            return Err(SetupError::cancelled());
          }
        },
        None => {
          return Err(SetupError::usage(format!(
            "a runner named '{name}' already exists in {url} — pass --replace to replace it"
          )));
        },
      }
    }
  }
  Ok(())
}

/// The linger answer: flag, service-step question, or off. Setup
/// decides; the installer never prompts (`LingerArgs::decided`).
pub(crate) fn resolve_linger(
  args: &SetupArgs,
  prompts: &mut Option<Prompts>,
  facts: &MachineFacts,
  service: bool,
) -> Result<bool, SetupError> {
  if !service || args.flow.enable_linger {
    return Ok(args.flow.enable_linger);
  }
  if facts.supervisor != "systemd" || facts.linger == Some(true) {
    return Ok(false);
  }
  match prompts.as_mut() {
    Some(prompts) => prompts
      .confirm(
        "Keep the service running after you log out (enable linger)?",
        true,
      )
      .map_err(SetupError::from),
    None => Ok(false),
  }
}
