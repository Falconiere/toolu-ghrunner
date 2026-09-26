//! Shared array identity and wildcard-filter metadata.

use std::ops::Deref;
use std::sync::Arc;

use crate::types::ExprValue;

/// An expression array whose clones retain reference identity.
#[derive(Debug, Clone, Default)]
pub struct ExprArray {
  values: Arc<Vec<ExprValue>>,
  /// Whether later indexes project over the wildcard-selected members.
  pub(crate) filtered: bool,
}

impl ExprArray {
  /// Build a wildcard result whose later indexes project over its members.
  pub(crate) fn filtered(values: Vec<ExprValue>) -> Self {
    Self {
      values: Arc::new(values),
      filtered: true,
    }
  }

  /// Compare storage identity without comparing contents.
  pub(crate) fn same_reference(&self, other: &Self) -> bool {
    Arc::ptr_eq(&self.values, &other.values)
  }
}

impl FromIterator<ExprValue> for ExprArray {
  fn from_iter<T: IntoIterator<Item = ExprValue>>(iter: T) -> Self {
    Self {
      values: Arc::new(iter.into_iter().collect()),
      filtered: false,
    }
  }
}

impl Deref for ExprArray {
  type Target = [ExprValue];
  fn deref(&self) -> &Self::Target {
    &self.values
  }
}

impl<'a> IntoIterator for &'a ExprArray {
  type Item = &'a ExprValue;
  type IntoIter = std::slice::Iter<'a, ExprValue>;
  fn into_iter(self) -> Self::IntoIter {
    self.iter()
  }
}

impl IntoIterator for ExprArray {
  type Item = ExprValue;
  type IntoIter = std::vec::IntoIter<ExprValue>;
  fn into_iter(self) -> Self::IntoIter {
    Arc::unwrap_or_clone(self.values).into_iter()
  }
}
