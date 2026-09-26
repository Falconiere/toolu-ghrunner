//! Shared insertion-ordered dictionaries for expression values.

use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use crate::types::ExprValue;
use indexmap::IndexMap;

/// An insertion-ordered expression object with copy-on-write snapshots.
#[derive(Debug, Clone, Default)]
pub struct ExprObject(Arc<IndexMap<String, ExprValue>>);

impl ExprObject {
  /// Compare storage identity without comparing contents.
  pub(crate) fn same_reference(&self, other: &Self) -> bool {
    Arc::ptr_eq(&self.0, &other.0)
  }
}

impl FromIterator<(String, ExprValue)> for ExprObject {
  fn from_iter<T: IntoIterator<Item = (String, ExprValue)>>(iter: T) -> Self {
    Self(Arc::new(iter.into_iter().collect()))
  }
}

impl Deref for ExprObject {
  type Target = IndexMap<String, ExprValue>;
  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

impl DerefMut for ExprObject {
  fn deref_mut(&mut self) -> &mut Self::Target {
    Arc::make_mut(&mut self.0)
  }
}

impl<'a> IntoIterator for &'a ExprObject {
  type Item = (&'a String, &'a ExprValue);
  type IntoIter = indexmap::map::Iter<'a, String, ExprValue>;
  fn into_iter(self) -> Self::IntoIter {
    self.iter()
  }
}

impl IntoIterator for ExprObject {
  type Item = (String, ExprValue);
  type IntoIter = indexmap::map::IntoIter<String, ExprValue>;
  fn into_iter(self) -> Self::IntoIter {
    Arc::unwrap_or_clone(self.0).into_iter()
  }
}
