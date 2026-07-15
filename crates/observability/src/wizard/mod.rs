//! Setup wizard: a pure state/input/verify core plus thin terminal and
//! render helpers for `toolu-runner`'s guided first-run flow. The reducers
//! do no I/O — the bin performs the real auth / register / install / verify
//! work and folds the outcomes through [`WizardState`].

/// Keyboard mapping: key events → high-level `Action`s.
pub mod input;
/// Pure reducer: `StepEvent`s → the wizard model.
pub mod state;
/// Testable alt-screen + cursor command writers.
pub mod term;
/// Ratatui rendering, pure view over `state::WizardState`.
pub mod ui;
/// Pure decision for the final "is the runner online?" stage.
pub mod verify;

pub use state::{SetupInputs, StepEvent, StepId, StepStatus, WizardState};
