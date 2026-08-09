# broken-no-manifest

Deliberately holds no `action.yml`/`action.yaml`. Used by
`composite_continue_on_error_test.rs` to force a hermetic
`RunnerError::ActionManifest` when a composite's nested `uses:` resolves
here — no network involved. This file exists only so git tracks the
directory (git does not track empty directories).
