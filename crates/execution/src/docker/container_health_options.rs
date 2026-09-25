//! Typed translation of Docker health-check options.

use bollard::models::{ContainerCreateBody, HealthConfig};
use shared::RunnerError;

/// Return whether an argv flag configures a Docker health check.
pub(super) fn is_health_option(flag: &str) -> bool {
  matches!(
    flag,
    "--health-cmd"
      | "--health-interval"
      | "--health-timeout"
      | "--health-retries"
      | "--health-start-period"
      | "--health-start-interval"
      | "--no-healthcheck"
  )
}

/// Apply health-check argv options to a Docker create request.
///
/// # Errors
///
/// Returns [`RunnerError::Config`] when options are malformed, out of range,
/// or disable a health check while also configuring one.
pub(super) fn apply_health_options(
  options: &[String],
  body: &mut ContainerCreateBody,
) -> Result<(), RunnerError> {
  let mut health = HealthConfig::default();
  let mut has_health_option = false;
  let mut disabled = false;
  let mut values = options.iter();

  while let Some(flag) = values.next() {
    if !is_health_option(flag) {
      skip_non_health_value(flag, &mut values)?;
      continue;
    }
    has_health_option = true;
    if flag == "--no-healthcheck" {
      disabled = true;
      continue;
    }
    let value = next_value(flag, &mut values)?;
    match flag.as_str() {
      "--health-cmd" => health.test = Some(vec!["CMD-SHELL".to_owned(), value]),
      "--health-interval" => health.interval = Some(parse_duration(flag, &value)?),
      "--health-timeout" => health.timeout = Some(parse_duration(flag, &value)?),
      "--health-retries" => health.retries = Some(parse_retries(&value)?),
      "--health-start-period" => health.start_period = Some(parse_duration(flag, &value)?),
      "--health-start-interval" => health.start_interval = Some(parse_duration(flag, &value)?),
      _ => return Err(option_error("unsupported health option")),
    }
  }

  if disabled && has_other_settings(&health) {
    return Err(option_error(
      "option `--no-healthcheck` cannot be combined with other health options",
    ));
  }
  if disabled {
    health.test = Some(vec!["NONE".to_owned()]);
  }
  if has_health_option {
    body.healthcheck = Some(health);
  }
  Ok(())
}

fn skip_non_health_value<'a>(
  flag: &str,
  values: &mut impl Iterator<Item = &'a String>,
) -> Result<(), RunnerError> {
  if takes_value(flag) {
    next_value(flag, values).map(|_value| ())?;
  }
  Ok(())
}

fn takes_value(flag: &str) -> bool {
  matches!(
    flag,
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
      | "--pids-limit"
      | "--ulimit"
      | "--user"
      | "--group-add"
      | "--hostname"
      | "--domainname"
      | "--cap-add"
      | "--cap-drop"
      | "--security-opt"
  )
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

fn has_other_settings(health: &HealthConfig) -> bool {
  health.interval.is_some()
    || health.timeout.is_some()
    || health.retries.is_some()
    || health.start_period.is_some()
    || health.start_interval.is_some()
    || health.test.is_some()
}

fn parse_duration(flag: &str, value: &str) -> Result<i64, RunnerError> {
  if value == "0" {
    return Ok(0);
  }
  if value.starts_with('-') {
    return Err(option_error(&format!(
      "option `{flag}` requires a non-negative Docker duration"
    )));
  }
  let value = value.strip_prefix('+').unwrap_or(value);
  if value.is_empty() {
    return Err(option_error(&format!(
      "option `{flag}` requires a Docker duration"
    )));
  }

  let mut rest = value;
  let mut total = 0_u128;
  while !rest.is_empty() {
    let (number, unit, next) = duration_component(rest)
      .ok_or_else(|| option_error(&format!("option `{flag}` requires a Docker duration")))?;
    let nanos = decimal_nanos(number, unit)
      .ok_or_else(|| option_error(&format!("option `{flag}` requires a Docker duration")))?;
    total = total
      .checked_add(nanos)
      .ok_or_else(|| option_error(&format!("option `{flag}` is out of range")))?;
    rest = next;
  }
  let nanos = i64::try_from(total)
    .map_err(|_error| option_error(&format!("option `{flag}` is out of range")))?;
  if nanos != 0 && nanos < 1_000_000 {
    return Err(option_error(&format!(
      "option `{flag}` must be zero or at least 1ms"
    )));
  }
  Ok(nanos)
}

fn duration_component(value: &str) -> Option<(&str, u128, &str)> {
  let number_end = value
    .bytes()
    .take_while(|byte| byte.is_ascii_digit() || *byte == b'.')
    .count();
  let number = value.get(..number_end)?;
  let after_number = value.get(number_end..)?;
  let (unit, multiplier) = duration_unit(after_number)?;
  Some((number, multiplier, after_number.strip_prefix(unit)?))
}

fn duration_unit(value: &str) -> Option<(&str, u128)> {
  for (unit, multiplier) in [
    ("ns", 1),
    ("us", 1_000),
    ("µs", 1_000),
    ("ms", 1_000_000),
    ("s", 1_000_000_000),
    ("m", 60_000_000_000),
    ("h", 3_600_000_000_000),
  ] {
    if value.starts_with(unit) {
      return Some((unit, multiplier));
    }
  }
  None
}

fn decimal_nanos(number: &str, unit: u128) -> Option<u128> {
  let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
  if whole.is_empty() && fraction.is_empty() || number.matches('.').count() > 1 {
    return None;
  }
  let whole = if whole.is_empty() {
    0
  } else {
    whole.parse::<u128>().ok()?
  };
  if !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
    return None;
  }
  let fraction = fraction_nanos(fraction, unit)?;
  whole.checked_mul(unit)?.checked_add(fraction)
}

fn fraction_nanos(fraction: &str, unit: u128) -> Option<u128> {
  let numerator = if fraction.is_empty() {
    0
  } else {
    fraction.parse::<u128>().ok()?
  };
  let denominator = (0..fraction.len()).try_fold(1_u128, |value, _index| value.checked_mul(10))?;
  numerator.checked_mul(unit)?.checked_div(denominator)
}

fn parse_retries(value: &str) -> Result<i64, RunnerError> {
  value
    .parse::<i64>()
    .ok()
    .filter(|value| *value >= 0)
    .ok_or_else(|| option_error("option `--health-retries` requires a non-negative integer"))
}

fn option_error(message: &str) -> RunnerError {
  RunnerError::Config(format!("invalid job container options: {message}"))
}

#[cfg(test)]
#[path = "tests/container_health_options.rs"]
mod tests;
