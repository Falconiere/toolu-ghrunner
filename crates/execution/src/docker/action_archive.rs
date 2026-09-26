//! Docker build-context archiving with Moby-compatible ignore patterns.
//! Upstream provenance and license notices: `docs/docker-ignore-notices.md`.

use std::fs;
use std::io::{Error, ErrorKind};
use std::path::{Component, Path, PathBuf};

use regex::Regex;

/// Archive a Docker context after applying its selected Docker ignore file.
pub(super) fn archive_context(context: &Path, dockerfile: &str) -> Result<Vec<u8>, Error> {
  let ignore_path = ignore_path(context, dockerfile);
  let matcher = DockerIgnore::read(ignore_path.as_deref())?;
  let mut builder = tar::Builder::new(Vec::new());
  builder.follow_symlinks(false);
  append_directory(
    &mut builder,
    context,
    Path::new(""),
    &matcher,
    dockerfile,
    ignore_path.as_deref(),
  )?;
  builder.into_inner()
}

fn ignore_path(context: &Path, dockerfile: &str) -> Option<PathBuf> {
  let specific = context.join(format!("{dockerfile}.dockerignore"));
  if specific.is_file() {
    Some(specific)
  } else {
    let default = context.join(".dockerignore");
    default.is_file().then_some(default)
  }
}

fn append_directory(
  builder: &mut tar::Builder<Vec<u8>>,
  context: &Path,
  relative_dir: &Path,
  matcher: &DockerIgnore,
  dockerfile: &str,
  ignore_path: Option<&Path>,
) -> Result<(), Error> {
  let directory = context.join(relative_dir);
  let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
  entries.sort_by_key(std::fs::DirEntry::file_name);
  for entry in entries {
    let relative = relative_dir.join(entry.file_name());
    let archive_path = slash_path(&relative);
    let entry_path = entry.path();
    let ignored = matcher.ignored(&archive_path);
    let control = archive_path == dockerfile || ignore_path.is_some_and(|path| path == entry_path);
    if control || !ignored {
      builder.append_path_with_name(&entry_path, &relative)?;
    }
    if entry.file_type()?.is_dir() && (!ignored || matcher.may_reinclude_descendant(&archive_path))
    {
      append_directory(
        builder,
        context,
        &relative,
        matcher,
        dockerfile,
        ignore_path,
      )?;
    }
  }
  Ok(())
}

fn slash_path(path: &Path) -> String {
  path
    .components()
    .filter_map(|component| match component {
      Component::Normal(value) => Some(value.to_string_lossy()),
      Component::Prefix(_) | Component::RootDir | Component::CurDir | Component::ParentDir => None,
    })
    .collect::<Vec<_>>()
    .join("/")
}

struct DockerIgnore {
  patterns: Vec<Pattern>,
}

impl DockerIgnore {
  fn read(path: Option<&Path>) -> Result<Self, Error> {
    let Some(path) = path else {
      return Ok(Self {
        patterns: Vec::new(),
      });
    };
    let bytes = fs::read(path)?;
    let text = String::from_utf8_lossy(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes));
    let mut patterns = Vec::new();
    for line in text.lines() {
      if line.starts_with('#') {
        continue;
      }
      let mut value = line.trim();
      if value.is_empty() {
        continue;
      }
      let exclusion = value.starts_with('!');
      if exclusion {
        value = value.strip_prefix('!').map(str::trim).unwrap_or_default();
        if value.is_empty() {
          return Err(Error::new(
            ErrorKind::InvalidInput,
            "illegal Docker ignore exclusion pattern: !",
          ));
        }
      }
      let value = clean_pattern(value);
      if value == "." {
        continue;
      }
      patterns.push(Pattern::compile(&value, exclusion)?);
    }
    Ok(Self { patterns })
  }

  fn ignored(&self, path: &str) -> bool {
    let mut ignored = false;
    for pattern in &self.patterns {
      if pattern.exclusion != ignored {
        continue;
      }
      let matched =
        pattern.matches(path) || parent_paths(path).any(|parent| pattern.matches(parent));
      if matched {
        ignored = !pattern.exclusion;
      }
    }
    ignored
  }

  fn may_reinclude_descendant(&self, directory: &str) -> bool {
    self
      .patterns
      .iter()
      .any(|pattern| pattern.exclusion && pattern.may_match_descendant(directory))
  }
}

fn clean_pattern(pattern: &str) -> String {
  let rooted = pattern.starts_with('/');
  let mut parts: Vec<&str> = Vec::new();
  for part in pattern.split('/') {
    match part {
      ".." if parts.last().is_some_and(|value| *value != "..") => {
        parts.pop();
      },
      ".." if !rooted => parts.push(part),
      "" | "." | ".." => {},
      value => parts.push(value),
    }
  }
  if parts.is_empty() {
    ".".to_owned()
  } else {
    parts.join("/")
  }
}

fn parent_paths(path: &str) -> impl Iterator<Item = &str> {
  path.match_indices('/').map(|(index, _)| &path[..index])
}

struct Pattern {
  exclusion: bool,
  literal_prefix: String,
  matcher: PatternMatch,
}

enum PatternMatch {
  Exact(String),
  Prefix(String),
  Suffix(String),
  Regex(Regex),
}

impl Pattern {
  fn compile(pattern: &str, exclusion: bool) -> Result<Self, Error> {
    let matcher = compile_match(pattern)?;
    let literal_prefix = literal_prefix(pattern);
    Ok(Self {
      exclusion,
      literal_prefix,
      matcher,
    })
  }

  fn matches(&self, path: &str) -> bool {
    match &self.matcher {
      PatternMatch::Exact(pattern) => path == pattern,
      PatternMatch::Prefix(prefix) => path.starts_with(prefix),
      PatternMatch::Suffix(suffix) => {
        path.ends_with(suffix)
          || suffix
            .strip_prefix('/')
            .is_some_and(|without_slash| path == without_slash)
      },
      PatternMatch::Regex(regex) => regex.is_match(path),
    }
  }

  fn may_match_descendant(&self, directory: &str) -> bool {
    if self.literal_prefix.is_empty() {
      return true;
    }
    self.literal_prefix == directory
      || self
        .literal_prefix
        .strip_prefix(directory)
        .is_some_and(|remainder| remainder.starts_with('/'))
      || directory.starts_with(&self.literal_prefix)
  }
}

fn literal_prefix(pattern: &str) -> String {
  let mut prefix = String::new();
  let mut chars = pattern.chars();
  while let Some(character) = chars.next() {
    match character {
      '*' | '?' | '[' => break,
      '\\' => {
        if let Some(escaped) = chars.next() {
          prefix.push(escaped);
        }
      },
      other => prefix.push(other),
    }
  }
  prefix.trim_end_matches('/').to_owned()
}

fn compile_match(pattern: &str) -> Result<PatternMatch, Error> {
  let compiled = PatternCompiler::new(pattern).compile()?;
  if compiled.suffix {
    return Ok(PatternMatch::Suffix(
      pattern.strip_prefix("**").unwrap_or(pattern).to_owned(),
    ));
  }
  if compiled.prefix {
    return Ok(PatternMatch::Prefix(
      pattern.strip_suffix("**").unwrap_or(pattern).to_owned(),
    ));
  }
  if !compiled.wildcard {
    return Ok(PatternMatch::Exact(pattern.to_owned()));
  }
  Regex::new(&format!("{}$", compiled.regex))
    .map(PatternMatch::Regex)
    .map_err(|error| Error::new(ErrorKind::InvalidInput, error))
}

struct PatternCompiler<'a> {
  chars: std::iter::Peekable<std::str::Chars<'a>>,
  regex: String,
  wildcard: bool,
  prefix: bool,
  suffix: bool,
  position: usize,
}

impl<'a> PatternCompiler<'a> {
  fn new(pattern: &'a str) -> Self {
    Self {
      chars: pattern.chars().peekable(),
      regex: "^".to_owned(),
      wildcard: false,
      prefix: false,
      suffix: false,
      position: 0,
    }
  }

  fn compile(mut self) -> Result<Self, Error> {
    while let Some(character) = self.chars.next() {
      self.append(character)?;
      self.position += 1;
    }
    Ok(self)
  }

  fn append(&mut self, character: char) -> Result<(), Error> {
    match character {
      '*' if self.chars.peek() == Some(&'*') => self.append_double_star(),
      '*' => {
        self.regex.push_str("[^/]*");
        self.wildcard = true;
        self.suffix = false;
      },
      '?' => {
        self.regex.push_str("[^/]");
        self.wildcard = true;
        self.suffix = false;
      },
      '\\' => self.append_escape()?,
      '[' => self.append_class()?,
      other => self.regex.push_str(&regex::escape(&other.to_string())),
    }
    Ok(())
  }

  fn append_double_star(&mut self) {
    self.chars.next();
    if self.chars.peek() == Some(&'/') {
      self.chars.next();
    }
    if self.chars.peek().is_none() {
      if self.wildcard {
        self.regex.push_str(".*");
      } else {
        self.prefix = true;
      }
    } else {
      self.regex.push_str("(.*/)?");
      self.wildcard = true;
    }
    self.suffix = self.position == 0;
  }

  fn append_escape(&mut self) -> Result<(), Error> {
    let next = self.chars.next().ok_or_else(bad_pattern)?;
    self.regex.push_str(&regex::escape(&next.to_string()));
    self.wildcard = true;
    self.suffix = false;
    Ok(())
  }

  fn append_class(&mut self) -> Result<(), Error> {
    self.regex.push('[');
    if self.chars.peek() == Some(&'^') {
      self.chars.next();
      self.regex.push('^');
    }
    let mut ranges = 0;
    loop {
      if self.chars.peek() == Some(&']') && ranges > 0 {
        self.chars.next();
        self.regex.push(']');
        break;
      }
      let (low, low_escaped) = self.class_character()?;
      append_class_character(&mut self.regex, low, low_escaped);
      if self.chars.peek() == Some(&'-') {
        self.chars.next();
        self.regex.push('-');
        let (high, high_escaped) = self.class_character()?;
        append_class_character(&mut self.regex, high, high_escaped);
      }
      ranges += 1;
    }
    self.wildcard = true;
    self.suffix = false;
    Ok(())
  }

  fn class_character(&mut self) -> Result<(char, bool), Error> {
    let character = self.chars.next().ok_or_else(bad_pattern)?;
    if matches!(character, '-' | ']') {
      return Err(bad_pattern());
    }
    if character == '\\' {
      return self
        .chars
        .next()
        .map(|escaped| (escaped, true))
        .ok_or_else(bad_pattern);
    }
    Ok((character, false))
  }
}

fn append_class_character(target: &mut String, character: char, escaped: bool) {
  if escaped {
    target.push_str(&regex::escape(&character.to_string()));
    return;
  }
  if matches!(character, '[' | ']' | '\\' | '&' | '~' | '^') {
    target.push('\\');
  }
  target.push(character);
}

fn bad_pattern() -> Error {
  Error::new(
    ErrorKind::InvalidInput,
    "syntax error in Docker ignore pattern",
  )
}

#[cfg(test)]
#[path = "tests/action_archive.rs"]
mod tests;
