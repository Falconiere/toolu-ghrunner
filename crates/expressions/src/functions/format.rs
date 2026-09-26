//! Format placeholders parsed before lazily evaluating referenced arguments.

use crate::types::ExprValue;
use shared::RunnerError;

enum Segment {
  Text(String),
  Argument {
    index: usize,
    invalid_specifier: bool,
  },
}

/// Render a format string, caching each referenced argument once.
pub(crate) fn render(
  template: &str,
  count: usize,
  mut argument: impl FnMut(usize) -> Result<ExprValue, RunnerError>,
) -> Result<ExprValue, RunnerError> {
  let segments = parse(template, count)?;
  let mut cache = vec![None; count];
  let mut result = String::new();
  for segment in segments {
    match segment {
      Segment::Text(text) => result.push_str(&text),
      Segment::Argument {
        index,
        invalid_specifier,
      } => {
        let cached = cache
          .get_mut(index)
          .ok_or_else(|| invalid("index out of range"))?;
        if cached.is_none() {
          *cached = Some(argument(index)?.coerce_to_string());
        }
        if invalid_specifier {
          return Err(invalid("unsupported format specifier"));
        }
        if let Some(value) = cached {
          result.push_str(value);
        }
      },
    }
  }
  Ok(ExprValue::String(result))
}

fn parse(template: &str, count: usize) -> Result<Vec<Segment>, RunnerError> {
  let mut chars = template.chars().peekable();
  let mut text = String::new();
  let mut segments = Vec::new();
  while let Some(ch) = chars.next() {
    if ch != '{' && ch != '}' {
      text.push(ch);
      continue;
    }
    if chars.peek() == Some(&ch) {
      chars.next();
      text.push(ch);
      continue;
    }
    if ch == '}' {
      return Err(invalid("unmatched closing brace"));
    }
    segments.push(Segment::Text(std::mem::take(&mut text)));
    let mut digits = String::new();
    while chars.peek().is_some_and(char::is_ascii_digit) {
      if let Some(digit) = chars.next() {
        digits.push(digit);
      }
    }
    let index = usize::from(
      digits
        .parse::<u8>()
        .map_err(|error| invalid(&format!("invalid argument index: {error}")))?,
    );
    if index >= count {
      return Err(invalid("argument index out of range"));
    }
    let invalid_specifier = match chars.next() {
      Some('}') => false,
      Some(':') => {
        let mut nonempty = false;
        loop {
          match chars.next() {
            Some('}') if chars.peek() == Some(&'}') => {
              chars.next();
              nonempty = true;
            },
            Some('}') => break,
            Some(_) => nonempty = true,
            None => return Err(invalid("unclosed format specifier")),
          }
        }
        nonempty
      },
      _ => return Err(invalid("unclosed or invalid placeholder")),
    };
    segments.push(Segment::Argument {
      index,
      invalid_specifier,
    });
  }
  segments.push(Segment::Text(text));
  Ok(segments)
}

fn invalid(message: &str) -> RunnerError {
  RunnerError::Expression(format!("format: {message}"))
}
