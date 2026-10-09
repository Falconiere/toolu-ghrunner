//! Display-name helpers against the expressions GitHub compiled for the
//! issue-99 capture (`step-attrs-99.yml`, run 37992025403) and the names
//! the reference runner showed for them in the jobs API.

use super::{References, display_string, first_line, references};

/// Reference step 24 kept its unevaluated job-start name verbatim.
#[test]
fn captured_format_unrolls_to_the_reference_name() {
  assert_eq!(
    display_string("format('Bad {0}', fromJSON(steps.prior.outputs.bad))"),
    "Bad ${{ fromJSON(steps.prior.outputs.bad) }}"
  );
  assert_eq!(
    display_string("format('Deferred {0}', steps.prior.outputs.label)"),
    "Deferred ${{ steps.prior.outputs.label }}"
  );
  assert_eq!(
    display_string("format('Matrix {0} input {1}', matrix.lane, inputs.who)"),
    "Matrix ${{ matrix.lane }} input ${{ inputs.who }}"
  );
  assert_eq!(
    display_string("format('echo prior {0}', steps.prior.outputs.label)"),
    "echo prior ${{ steps.prior.outputs.label }}"
  );
}

#[test]
fn non_format_expressions_show_the_whole_expression() {
  assert_eq!(
    display_string("fromJSON('nope')"),
    "${{ fromJSON('nope') }}"
  );
  assert_eq!(display_string("format('x')"), "${{ format('x') }}");
  assert_eq!(
    display_string("format(matrix.template, 1)"),
    "${{ format(matrix.template, 1) }}"
  );
}

#[test]
fn unrenderable_format_falls_back_to_the_expression() {
  assert_eq!(
    display_string("format('{1}', matrix.a)"),
    "${{ format('{1}', matrix.a) }}"
  );
}

/// Parameters print the way upstream's `ConvertToExpression` does.
#[test]
fn parameters_print_as_upstream_expressions() {
  assert_eq!(
    display_string("format('{0}', github['sha'])"),
    "${{ github.sha }}"
  );
  assert_eq!(
    display_string("format('{0}', matrix['my key'])"),
    "${{ matrix['my key'] }}"
  );
  assert_eq!(display_string("format('{0}', 'it''s')"), "${{ 'it''s' }}");
  assert_eq!(
    display_string("format('{0} {1}', !inputs.a, null)"),
    "${{ !inputs.a }} ${{ null }}"
  );
}

/// A lone parenthesized group loses its parentheses; nested groups keep them.
#[test]
fn single_group_parameters_drop_their_parentheses() {
  assert_eq!(
    display_string("format('x {0}', matrix.a || matrix.b)"),
    "x ${{ matrix.a || matrix.b }}"
  );
  assert_eq!(
    display_string("format('x {0}', matrix.a || matrix.b && matrix.c)"),
    "x ${{ (matrix.a || (matrix.b && matrix.c)) }}"
  );
}

#[test]
fn first_line_trims_leading_whitespace_and_later_lines() {
  assert_eq!(first_line(" \t\r\n echo a\r\necho b"), "echo a");
  assert_eq!(first_line("single"), "single");
  assert_eq!(first_line("\n\n"), "");
}

#[test]
fn references_lists_contexts_and_functions_as_written() -> Result<(), shared::RunnerError> {
  assert_eq!(
    references("format('Bad {0}', fromJSON(steps.prior.outputs.bad))")?,
    References {
      contexts: vec!["steps".to_owned()],
      functions: vec!["format".to_owned(), "fromJSON".to_owned()],
    }
  );
  assert_eq!(
    references("matrix[inputs.key] && success()")?,
    References {
      contexts: vec!["matrix".to_owned(), "inputs".to_owned()],
      functions: vec!["success".to_owned()],
    }
  );
  assert!(references("format(").is_err());
  Ok(())
}
