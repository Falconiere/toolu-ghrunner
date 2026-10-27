# setup

Belongs: the `toolu-runner setup` onboarding flow (#204) — machine check, job source, target, auth, scope, runner details, service, Review, apply, and online confirmation, plus the re-run menu — and its toolu.sh branch (#207): pairing, enrollment, refresh, first poll.

Doesn't belong: registration minting (`register_cmd`), App creation internals (`create_app_cmd`/`app_import`), service unit rendering (`machine_service`/`service_cmd`), or the toolu.sh `/runner/*` calls (`wire::net::toolu_*`) and a paired machine's shared helpers (`toolu_host`).

| File | What |
| --- | --- |
| `args.rs` | `SetupArgs`: every prompt's flag (epic #199 table). |
| `plan.rs` | `ResolvedPlan`/`Target`/`AuthPlan`, flag grammar, defaults, exit-2 naming. |
| `prompts.rs` | `/dev/tty` inline prompts (raw mode via `crossterm`, own key parser), terminal restore on cancel/panic/signal. |
| `render.rs` | Legend output: rail + glyphs, or plain `setup:` lines with no TTY/`NO_COLOR`. |
| `machine.rs` | Machine check facts (platform, supervisor, linger, headless, existing registrations). |
| `github.rs` | Live reads: org check, runner groups, repo lists, token permission probes, online polling. |
| `stage.rs` | Plan staging: target → auth → scope → details → service. |
| `scope.rs` | Runner group / repository picker, name, collision, linger. |
| `apply.rs` | The one write path: token store, registrations, machine unit, online verify; idempotent resume. |
| `reconfigure.rs` | Re-run menu's Reconfigure: a fresh plan derived from the picked registration's own config, caller flags winning. |
| `toolu.rs` | toolu.sh branch (#207): source flags, GitHub-only flag refusal, pair / enroll, persist `<home>/toolu/<org>/` (0600 token), refresh via `PATCH /runner/host`, service + first poll, dry-run Review. |
| `toolu_wait.rs` | Machine report, device-code wait (interval, `slow_down`, countdown line, expiry). |
| `tty_lines.rs` | The `/dev/tty` reader thread, and lending its input to `create-app`'s waits (#205: the paste and line commands inside setup). |
| `tests/` | Unit tests per module (flat, included via `#[path]`). |
