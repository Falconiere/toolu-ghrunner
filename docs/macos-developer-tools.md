# macOS Developer Tools setting (XProtect build stalls)

macOS-only. Linux and Windows contributors can skip this page.

macOS XProtect scans every newly created executable on its first launch, and
the scanning service handles one file at a time. A Rust build creates many new
executables: build scripts, proc-macro dylibs loaded by rustc, and test
binaries. On a Mac where the scan is active, builds and tests mostly wait on
the scan rather than on the compiler.

## Symptom

- `syspolicyd` and `XprotectService` are busy (Activity Monitor, or `top`)
  while `cargo build` / `cargo test` runs, and the load average stays low for
  the core count.
- In `cargo build --timings`, build-script bars (`thiserror`, `httparse`,
  `crunchy`, `proc-macro2`) run for tens of seconds although the scripts
  finish in milliseconds.
- A cold `cargo test` spends minutes before the first tests start, while
  relaunching an already-run test binary is instant.

Reference measurement (Apple M4, 10 cores, `2ca6459`): clean
`cargo build --workspace` took 382 s; 49 fresh test binaries took ~1,248 s for
16.5 s of actual test time.

## Setting

1. Open System Settings → Privacy & Security → Developer Tools.
2. Add the terminal app you build from (Terminal, iTerm, Ghostty, …) and
   enable it.
3. If the Developer Tools entry is missing, run
   `sudo spctl developer-mode enable-terminal`, then reopen the pane.
4. Quit and restart the terminal. Processes started before the restart keep
   the old policy.

## Trade-off

This turns off the scan for everything launched from that terminal, not just
this repo. Whether to do it is each developer's decision. No script, hook or
`tools/check.sh` step in this repo changes the setting, and none should.

## Verification

Compare a clean build before and after the change:

```sh
cargo clean && cargo build --workspace --timings
```

Open `target/cargo-timings/cargo-timing.html`: build-script bars should drop
from tens of seconds to well under a second. Alternatively, time the first
launch of a freshly linked test binary; with the setting active it is
comparable to a relaunch.

## References

- Nicholas Nethercote, [Faster Rust builds on Mac](https://nnethercote.github.io/2025/09/04/faster-rust-builds-on-mac.html)
- [rust-lang/cargo#15908](https://github.com/rust-lang/cargo/issues/15908)
- [cargo-nextest macOS notes](https://nexte.st/docs/installation/macos/)
