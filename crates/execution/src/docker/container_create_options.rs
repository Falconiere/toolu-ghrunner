//! Translation of validated job-container options into Docker API create fields.

use bollard::models::{ContainerCreateBody, HostConfig, ResourcesUlimits};
use shared::RunnerError;

use super::container_health_options::{apply_health_options, is_health_option};

/// Apply validated job-container option argv to a Docker create request.
///
/// The input must have been produced by
/// [`super::container_options::parse_container_options`]. This second
/// validation makes the boundary safe for direct callers too.
///
/// # Errors
///
/// Returns [`RunnerError::Config`] when an option value cannot be represented
/// by Docker's typed create API.
pub(crate) fn apply_container_options(
  options: &[String],
  body: &mut ContainerCreateBody,
) -> Result<(), RunnerError> {
  apply_health_options(options, body)?;
  let mut values = options.iter();
  while let Some(flag) = values.next() {
    if is_health_option(flag) {
      if flag != "--no-healthcheck" {
        next_value(flag, &mut values)?;
      }
    } else {
      apply_option(flag, &mut values, body)?;
    }
  }
  Ok(())
}

fn apply_option<'a>(
  flag: &str,
  values: &mut impl Iterator<Item = &'a String>,
  body: &mut ContainerCreateBody,
) -> Result<(), RunnerError> {
  match flag {
    "--privileged" | "--read-only" | "--user" | "--hostname" | "--domainname" => {
      apply_identity(flag, values, body)
    },
    "--cpus"
    | "--cpu-shares"
    | "--cpu-period"
    | "--cpu-quota"
    | "--cpuset-cpus"
    | "--cpuset-mems"
    | "--memory"
    | "--memory-reservation"
    | "--memory-swap"
    | "--memory-swappiness"
    | "--pids-limit" => apply_resource(flag, values, host_config(body)),
    "--group-add" | "--cap-add" | "--cap-drop" | "--security-opt" | "--ulimit" => {
      apply_security(flag, values, host_config(body))
    },
    _ => Err(option_error("unsupported option")),
  }
}

fn apply_identity<'a>(
  flag: &str,
  values: &mut impl Iterator<Item = &'a String>,
  body: &mut ContainerCreateBody,
) -> Result<(), RunnerError> {
  match flag {
    "--privileged" => host_config(body).privileged = Some(true),
    "--read-only" => host_config(body).readonly_rootfs = Some(true),
    "--user" => body.user = Some(next_value(flag, values)?),
    "--hostname" => body.hostname = Some(next_value(flag, values)?),
    "--domainname" => body.domainname = Some(next_value(flag, values)?),
    _ => return Err(option_error("unsupported identity option")),
  }
  Ok(())
}

fn apply_resource<'a>(
  flag: &str,
  values: &mut impl Iterator<Item = &'a String>,
  config: &mut HostConfig,
) -> Result<(), RunnerError> {
  let value = next_value(flag, values)?;
  match flag {
    "--cpus" => config.nano_cpus = Some(parse_nano_cpus(&value)?),
    "--cpu-shares" => config.cpu_shares = Some(parse_number(flag, &value)?),
    "--cpu-period" => config.cpu_period = Some(parse_number(flag, &value)?),
    "--cpu-quota" => config.cpu_quota = Some(parse_number(flag, &value)?),
    "--cpuset-cpus" => config.cpuset_cpus = Some(value),
    "--cpuset-mems" => config.cpuset_mems = Some(value),
    "--memory" => config.memory = Some(parse_memory(flag, &value)?),
    "--memory-reservation" => config.memory_reservation = Some(parse_memory(flag, &value)?),
    "--memory-swap" => config.memory_swap = Some(parse_memory(flag, &value)?),
    "--memory-swappiness" => config.memory_swappiness = Some(parse_number(flag, &value)?),
    "--pids-limit" => config.pids_limit = Some(parse_number(flag, &value)?),
    _ => return Err(option_error("unsupported resource option")),
  }
  Ok(())
}

fn apply_security<'a>(
  flag: &str,
  values: &mut impl Iterator<Item = &'a String>,
  config: &mut HostConfig,
) -> Result<(), RunnerError> {
  let value = next_value(flag, values)?;
  match flag {
    "--group-add" => push(&mut config.group_add, value),
    "--cap-add" => push(&mut config.cap_add, value),
    "--cap-drop" => push(&mut config.cap_drop, value),
    "--security-opt" => push(&mut config.security_opt, value),
    "--ulimit" => push_ulimit(config, &value)?,
    _ => return Err(option_error("unsupported security option")),
  }
  Ok(())
}

fn host_config(body: &mut ContainerCreateBody) -> &mut HostConfig {
  body.host_config.get_or_insert_with(HostConfig::default)
}

fn next_value<'a>(
  flag: &str,
  values: &mut impl Iterator<Item = &'a String>,
) -> Result<String, RunnerError> {
  values
    .next()
    .cloned()
    .ok_or_else(|| option_error(&format!("option `{flag}` requires a value")))
}

fn push(values: &mut Option<Vec<String>>, value: String) {
  values.get_or_insert_with(Vec::new).push(value);
}

fn parse_number(flag: &str, value: &str) -> Result<i64, RunnerError> {
  value
    .parse()
    .map_err(|_error| option_error(&format!("option `{flag}` requires an integer")))
}

fn parse_nano_cpus(value: &str) -> Result<i64, RunnerError> {
  let (whole, fraction) = match value.split_once('.') {
    Some(parts) => parts,
    None => (value, ""),
  };
  if whole.is_empty()
    || !whole.bytes().all(|byte| byte.is_ascii_digit())
    || fraction.len() > 9
    || !fraction.bytes().all(|byte| byte.is_ascii_digit())
  {
    return Err(option_error("option `--cpus` requires a positive decimal"));
  }
  let whole = parse_number("--cpus", whole)?;
  let padded = format!("{fraction:0<9}");
  let fraction = parse_number("--cpus", &padded)?;
  whole
    .checked_mul(1_000_000_000)
    .and_then(|whole| whole.checked_add(fraction))
    .filter(|value| *value > 0)
    .ok_or_else(|| option_error("option `--cpus` is out of range"))
}

fn parse_memory(flag: &str, value: &str) -> Result<i64, RunnerError> {
  let lower = value.to_ascii_lowercase();
  let (number, multiplier) = match lower.as_str() {
    value if value.ends_with("kb") => (&value[..value.len() - 2], 1_024),
    value if value.ends_with("mb") => (&value[..value.len() - 2], 1_048_576),
    value if value.ends_with("gb") => (&value[..value.len() - 2], 1_073_741_824),
    value if value.ends_with('k') => (&value[..value.len() - 1], 1_024),
    value if value.ends_with('m') => (&value[..value.len() - 1], 1_048_576),
    value if value.ends_with('g') => (&value[..value.len() - 1], 1_073_741_824),
    value => (value, 1),
  };
  parse_number(flag, number)?
    .checked_mul(multiplier)
    .ok_or_else(|| option_error(&format!("option `{flag}` is out of range")))
}

fn push_ulimit(config: &mut HostConfig, value: &str) -> Result<(), RunnerError> {
  let Some((name, limits)) = value.split_once('=') else {
    return Err(option_error("option `--ulimit` must use name=soft:hard"));
  };
  let Some((soft, hard)) = limits.split_once(':') else {
    return Err(option_error("option `--ulimit` must use name=soft:hard"));
  };
  let limit = ResourcesUlimits {
    name: Some(name.to_owned()),
    soft: Some(parse_number("--ulimit", soft)?),
    hard: Some(parse_number("--ulimit", hard)?),
  };
  config.ulimits.get_or_insert_with(Vec::new).push(limit);
  Ok(())
}

fn option_error(message: &str) -> RunnerError {
  RunnerError::Config(format!("invalid job container options: {message}"))
}

#[cfg(test)]
#[path = "tests/container_create_options.rs"]
mod tests;
