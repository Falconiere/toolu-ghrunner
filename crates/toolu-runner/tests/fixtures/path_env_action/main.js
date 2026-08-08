// Main stage: exports a PATH entry and an env var for later steps, exactly
// how `@actions/core`'s `addPath`/`exportVariable` do it — appending to the
// files named by $GITHUB_PATH / $GITHUB_ENV (never the stdout
// `::add-path::`/`::set-env::` commands, which real runners disabled after
// CVE-2020-15228).
const fs = require('fs');

// FIX (regression hardening): if the runner's node-stage wiring that creates
// and injects $GITHUB_PATH/$GITHUB_ENV is ever deleted, this stage must fail
// LOUDLY, naming the missing contract — not incidentally, via a bare Node
// "path must be a string" TypeError (undefined var) or a downstream
// `fixture-tool: command not found` in the NEXT step (unset var / file never
// created). Both env vars are streamed to `Log` events on this process's
// stderr (see `execute_node_action`), so a thrown Error here surfaces in the
// job's collected log lines and fails the test at the mechanism.
function requireFileCommandVar(name) {
  const path = process.env[name];
  if (!path) {
    throw new Error(
      `FIXTURE CONTRACT VIOLATION: ${name} is not set — the runner did not ` +
        'inject the file-command env for this node action stage'
    );
  }
  try {
    fs.accessSync(path, fs.constants.W_OK);
  } catch (err) {
    throw new Error(
      `FIXTURE CONTRACT VIOLATION: ${name}=${path} is not a writable file — ` +
        `the runner did not create the file-command file for this node ` +
        `action stage (${err.message})`
    );
  }
}

requireFileCommandVar('GITHUB_PATH');
requireFileCommandVar('GITHUB_ENV');

const binDir = process.env.INPUT_BIN_DIR;
fs.appendFileSync(process.env.GITHUB_PATH, `${binDir}\n`);
fs.appendFileSync(process.env.GITHUB_ENV, 'FIXTURE_GREETING=hello-from-action\n');

// A heredoc-form GITHUB_ENV value too, so the fix exercises both forms this
// action stage now has access to.
fs.appendFileSync(
  process.env.GITHUB_ENV,
  'FIXTURE_MULTILINE<<__FIXTURE_EOF__\nline one\nline two\n__FIXTURE_EOF__\n'
);
