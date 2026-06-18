//! Tests for `topological_sort` — the orchestrator's cycle-prevention
//! and execution-order logic.
//!
//! Each test builds a `HashMap<String, Vec<String>>` representing job
//! `needs:` declarations and verifies the sort output or error.

use std::collections::HashMap;

use toolu_runner::execution::workflow::job_graph::topological_sort;

#[test]
fn linear_chain_is_sorted_in_order() {
  let mut jobs = HashMap::new();
  jobs.insert("a".to_owned(), vec![]);
  jobs.insert("b".to_owned(), vec!["a".to_owned()]);
  jobs.insert("c".to_owned(), vec!["b".to_owned()]);
  jobs.insert("d".to_owned(), vec!["c".to_owned()]);

  let sorted = topological_sort(&jobs).expect("no cycle");
  // a must come before b, b before c, c before d
  let pos = |name: &str| sorted.iter().position(|j| j == name).unwrap();
  assert!(pos("a") < pos("b"), "a before b");
  assert!(pos("b") < pos("c"), "b before c");
  assert!(pos("c") < pos("d"), "c before d");
}

#[test]
fn diamond_dependency_produces_valid_order() {
  let mut jobs = HashMap::new();
  jobs.insert("a".to_owned(), vec![]);
  jobs.insert("b".to_owned(), vec!["a".to_owned()]);
  jobs.insert("c".to_owned(), vec!["a".to_owned()]);
  jobs.insert("d".to_owned(), vec!["b".to_owned(), "c".to_owned()]);

  let sorted = topological_sort(&jobs).expect("no cycle");
  let pos = |name: &str| sorted.iter().position(|j| j == name).unwrap();
  assert!(pos("a") < pos("b"), "a before b");
  assert!(pos("a") < pos("c"), "a before c");
  assert!(pos("b") < pos("d"), "b before d");
  assert!(pos("c") < pos("d"), "c before d");
}

#[test]
fn rejects_cycle_direct() {
  let mut jobs = HashMap::new();
  jobs.insert("a".to_owned(), vec!["b".to_owned()]);
  jobs.insert("b".to_owned(), vec!["a".to_owned()]);

  let err = topological_sort(&jobs).expect_err("cycle should error");
  let msg = format!("{err}");
  assert!(
    msg.to_lowercase().contains("cycle"),
    "expected cycle error, got: {msg}"
  );
}

#[test]
fn rejects_cycle_indirect() {
  let mut jobs = HashMap::new();
  jobs.insert("a".to_owned(), vec!["b".to_owned()]);
  jobs.insert("b".to_owned(), vec!["c".to_owned()]);
  jobs.insert("c".to_owned(), vec!["a".to_owned()]);

  let err = topological_sort(&jobs).expect_err("cycle should error");
  let msg = format!("{err}");
  assert!(
    msg.to_lowercase().contains("cycle"),
    "expected cycle error, got: {msg}"
  );
}

#[test]
fn empty_input_returns_empty() {
  let jobs: HashMap<String, Vec<String>> = HashMap::new();
  let sorted = topological_sort(&jobs).expect("empty input");
  assert!(sorted.is_empty());
}

#[test]
fn single_job_with_no_deps() {
  let mut jobs = HashMap::new();
  jobs.insert("only".to_owned(), vec![]);
  let sorted = topological_sort(&jobs).expect("single job");
  assert_eq!(sorted, vec!["only"]);
}

#[test]
fn independent_jobs_in_any_order() {
  let mut jobs = HashMap::new();
  jobs.insert("x".to_owned(), vec![]);
  jobs.insert("y".to_owned(), vec![]);
  jobs.insert("z".to_owned(), vec![]);

  let sorted = topological_sort(&jobs).expect("independent jobs");
  assert_eq!(sorted.len(), 3);
  for name in &["x", "y", "z"] {
    assert!(sorted.contains(&name.to_string()), "missing {name}");
  }
}
