//! Invariant Actions numeric coercion and G15 rendering.

/// Parse using the invariant runner coercion rules.
///
/// Invalid values map to NaN because Actions coercion is value-level, not a
/// runtime error path.
pub(crate) fn parse_number(input: &str) -> f64 {
  parse_number_checked(input).unwrap_or(f64::NAN)
}

/// Parse a numeric literal while retaining a reason suitable for lexer diagnostics.
pub(crate) fn parse_number_checked(input: &str) -> Result<f64, String> {
  let text = input.trim();
  if text.is_empty() {
    return Ok(0.0);
  }
  if let Some(digits) = text.strip_prefix("0x") {
    return parse_radix(digits, 16);
  }
  if let Some(digits) = text.strip_prefix("0o") {
    return parse_radix(digits, 8);
  }
  if text.eq_ignore_ascii_case("infinity") || text.eq_ignore_ascii_case("+infinity") {
    return Ok(f64::INFINITY);
  }
  if text.eq_ignore_ascii_case("-infinity") {
    return Ok(f64::NEG_INFINITY);
  }
  // Rust accepts `inf`; the reference's invariant number parser does not.
  if text
    .bytes()
    .any(|c| !c.is_ascii_digit() && !b"+-.eE".contains(&c))
  {
    return Err("invalid numeric syntax".to_owned());
  }
  text.parse::<f64>().map_err(|err| err.to_string())
}

fn parse_radix(digits: &str, radix: u32) -> Result<f64, String> {
  if digits.is_empty() || !digits.chars().all(|ch| ch.is_digit(radix)) {
    return Err("invalid radix digits".to_owned());
  }
  u32::from_str_radix(digits, radix)
    .map(|n| f64::from(i32::from_ne_bytes(n.to_ne_bytes())))
    .map_err(|err| err.to_string())
}

/// Render fifteen significant digits using invariant general formatting.
pub(crate) fn format_number(number: f64) -> String {
  if number.is_nan() {
    return "NaN".to_owned();
  }
  if number.is_infinite() {
    return if number.is_sign_negative() {
      "-Infinity"
    } else {
      "Infinity"
    }
    .to_owned();
  }
  let scientific = format!("{number:.14e}");
  let Some((mantissa, exponent)) = scientific.split_once('e') else {
    return scientific;
  };
  let Ok(exponent) = exponent.parse::<i32>() else {
    // This is derived from Rust's own scientific formatter; preserve its
    // numerically equivalent output if that internal shape ever changes.
    return scientific;
  };
  let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');
  if !(-4..15).contains(&exponent) {
    return format!("{mantissa}E{exponent:+03}");
  }
  let (sign, unsigned) = mantissa
    .strip_prefix('-')
    .map_or(("", mantissa), |s| ("-", s));
  let digits = unsigned.replace('.', "");
  let point = exponent + 1;
  if point <= 0 {
    let Ok(zeros) = usize::try_from(-point) else {
      return scientific;
    };
    return format!("{sign}0.{}{digits}", "0".repeat(zeros));
  }
  let Ok(point) = usize::try_from(point) else {
    return scientific;
  };
  if point >= digits.len() {
    return format!("{sign}{digits}{}", "0".repeat(point - digits.len()));
  }
  let (Some(left), Some(right)) = (digits.get(..point), digits.get(point..)) else {
    return scientific;
  };
  format!("{sign}{left}.{right}")
}
