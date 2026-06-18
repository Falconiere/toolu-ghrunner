use shared::RunnerError;

/// Token produced by the expression lexer.
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
  Ident(String),
  StringLit(String),
  NumberLit(f64),
  BoolLit(bool),
  Null,
  Dot,
  LBracket,
  RBracket,
  LParen,
  RParen,
  Comma,
  Star,
  Eq,
  Neq,
  Lt,
  Le,
  Gt,
  Ge,
  And,
  Or,
  Not,
}

/// Tokenize a GitHub Actions expression string.
///
/// Preserves original casing in `Ident` tokens — downstream matching
/// (evaluator, functions) must be case-insensitive.
///
/// # Errors
///
/// Returns `RunnerError::Expression` on unterminated strings, invalid
/// numbers, or unexpected characters.
pub fn lex(input: &str) -> Result<Vec<Token>, RunnerError> {
  let mut tokens = Vec::new();
  let bytes = input.as_bytes();
  let mut i = 0;

  while i < bytes.len() {
    let Some(&ch) = bytes.get(i) else {
      break;
    };

    if ch.is_ascii_whitespace() {
      i += 1;
      continue;
    }

    if ch == b'\'' {
      i += 1;
      let s = lex_string(bytes, &mut i)?;
      tokens.push(Token::StringLit(s));
    } else if ch.is_ascii_digit() {
      let n = lex_number(bytes, &mut i)?;
      tokens.push(Token::NumberLit(n));
    } else if is_ident_start(ch) {
      let word = lex_ident(bytes, &mut i);
      tokens.push(keyword_or_ident(word));
    } else {
      let tok = lex_operator(bytes, &mut i)?;
      tokens.push(tok);
    }
  }

  Ok(tokens)
}

fn lex_operator(bytes: &[u8], i: &mut usize) -> Result<Token, RunnerError> {
  let ch = bytes
    .get(*i)
    .copied()
    .ok_or_else(|| RunnerError::Expression("unexpected end of input".to_owned()))?;
  let next = bytes.get(*i + 1).copied();

  let (tok, advance) = match (ch, next) {
    (b'=', Some(b'=')) => (Token::Eq, 2),
    (b'!', Some(b'=')) => (Token::Neq, 2),
    (b'<', Some(b'=')) => (Token::Le, 2),
    (b'>', Some(b'=')) => (Token::Ge, 2),
    (b'&', Some(b'&')) => (Token::And, 2),
    (b'|', Some(b'|')) => (Token::Or, 2),
    (b'!', _) => (Token::Not, 1),
    (b'<', _) => (Token::Lt, 1),
    (b'>', _) => (Token::Gt, 1),
    (b'.', _) => (Token::Dot, 1),
    (b'(', _) => (Token::LParen, 1),
    (b')', _) => (Token::RParen, 1),
    (b'[', _) => (Token::LBracket, 1),
    (b']', _) => (Token::RBracket, 1),
    (b',', _) => (Token::Comma, 1),
    (b'*', _) => (Token::Star, 1),
    _ => {
      return Err(RunnerError::Expression(format!(
        "unexpected character: '{}'",
        char::from(ch)
      )));
    },
  };

  *i += advance;
  Ok(tok)
}

fn keyword_or_ident(word: String) -> Token {
  match word.as_str() {
    "true" => Token::BoolLit(true),
    "false" => Token::BoolLit(false),
    "null" => Token::Null,
    _ => Token::Ident(word),
  }
}

fn is_ident_start(ch: u8) -> bool {
  ch.is_ascii_alphabetic() || ch == b'_'
}

fn is_ident_continue(ch: u8) -> bool {
  ch.is_ascii_alphanumeric() || ch == b'_' || ch == b'-'
}

fn lex_string(bytes: &[u8], i: &mut usize) -> Result<String, RunnerError> {
  let mut s = String::new();

  loop {
    let Some(&ch) = bytes.get(*i) else {
      return Err(RunnerError::Expression(
        "unterminated string literal".to_owned(),
      ));
    };

    if ch == b'\'' {
      *i += 1;
      // Escaped quote: '' → '
      if bytes.get(*i).copied() == Some(b'\'') {
        s.push('\'');
        *i += 1;
      } else {
        return Ok(s);
      }
    } else {
      s.push(char::from(ch));
      *i += 1;
    }
  }
}

fn lex_number(bytes: &[u8], i: &mut usize) -> Result<f64, RunnerError> {
  let start = *i;
  let mut has_dot = false;

  while let Some(&ch) = bytes.get(*i) {
    if ch.is_ascii_digit() {
      *i += 1;
    } else if ch == b'.' && !has_dot {
      has_dot = true;
      *i += 1;
    } else {
      break;
    }
  }

  let s = std::str::from_utf8(bytes.get(start..*i).unwrap_or_default()).unwrap_or_default();

  s.parse::<f64>()
    .map_err(|e| RunnerError::Expression(format!("invalid number '{s}': {e}")))
}

fn lex_ident(bytes: &[u8], i: &mut usize) -> String {
  let start = *i;

  while let Some(&ch) = bytes.get(*i) {
    if is_ident_continue(ch) {
      *i += 1;
    } else {
      break;
    }
  }

  String::from_utf8_lossy(bytes.get(start..*i).unwrap_or_default()).into_owned()
}
