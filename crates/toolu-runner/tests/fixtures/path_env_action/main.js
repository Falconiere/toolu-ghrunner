// Main stage: exports a PATH entry and an env var for later steps, exactly
// how `@actions/core`'s `addPath`/`exportVariable` do it — appending to the
// files named by $GITHUB_PATH / $GITHUB_ENV (never the stdout
// `::add-path::`/`::set-env::` commands, which real runners disabled after
// CVE-2020-15228).
const fs = require('fs');

const binDir = process.env.INPUT_BIN_DIR;
fs.appendFileSync(process.env.GITHUB_PATH, `${binDir}\n`);
fs.appendFileSync(process.env.GITHUB_ENV, 'FIXTURE_GREETING=hello-from-action\n');

// A heredoc-form GITHUB_ENV value too, so the fix exercises both forms this
// action stage now has access to.
fs.appendFileSync(
  process.env.GITHUB_ENV,
  'FIXTURE_MULTILINE<<__FIXTURE_EOF__\nline one\nline two\n__FIXTURE_EOF__\n'
);
