//! Invariant Actions numeric coercion and G15 rendering.

/// Parse using the invariant runner coercion rules.
pub(crate) fn parse_number(input: &str) -> f64 {
  let text = input.trim();
  if text.is_empty() {
    return 0.0;
  }
  if let Some(digits) = text.strip_prefix("0x") {
    return parse_radix(digits, 16);
  }
  if let Some(digits) = text.strip_prefix("0o") {
    return parse_radix(digits, 8);
  }
  if text.eq_ignore_ascii_case("infinity") || text.eq_ignore_ascii_case("+infinity") {
    return f64::INFINITY;
  }
  if text.eq_ignore_ascii_case("-infinity") {
    return f64::NEG_INFINITY;
  }
  // Rust accepts `inf`; the reference's invariant number parser does not.
  if text
    .bytes()
    .any(|c| !c.is_ascii_digit() && !b"+-.eE".contains(&c))
  {
    return f64::NAN;
  }
  text.parse().unwrap_or(f64::NAN)
}

fn parse_radix(digits: &str, radix: u32) -> f64 {
  if digits.is_empty() || !digits.chars().all(|ch| ch.is_digit(radix)) {
    return f64::NAN;
  }
  u32::from_str_radix(digits, radix)
    .map(|n| f64::from(i32::from_ne_bytes(n.to_ne_bytes())))
    .unwrap_or(f64::NAN)
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
  let exponent: i32 = exponent.parse().unwrap_or(0);
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
    let zeros = usize::try_from(-point).unwrap_or(0);
    return format!("{sign}0.{}{digits}", "0".repeat(zeros));
  }
  let point = usize::try_from(point).unwrap_or(0);
  if point >= digits.len() {
    return format!("{sign}{digits}{}", "0".repeat(point - digits.len()));
  }
  format!(
    "{sign}{}.{}",
    digits.get(..point).unwrap_or_default(),
    digits.get(point..).unwrap_or_default()
  )
}
