// Dump the ACTIONS_* variables this stage received as KEY=VALUE lines to a
// file (rename-published), optionally holding the job open until a sentinel
// file appears. The `__DUMP__` / `__SENTINEL__` markers are filled in by
// `tests/helpers/node_probe.rs`; stdout carries the same set as one JSON line.
const fs = require('fs');
const seen = {};
for (const [key, value] of Object.entries(process.env)) {
  if (key.startsWith('ACTIONS_')) seen[key] = value;
}
console.log('PROBE ' + JSON.stringify(seen));
const dump = '__DUMP__';
if (dump) {
  const body = Object.entries(seen).map(([k, v]) => `${k}=${v}\n`).join('');
  fs.writeFileSync(dump + '.tmp', body);
  fs.renameSync(dump + '.tmp', dump);
}
const sentinel = '__SENTINEL__';
if (sentinel) {
  const deadline = Date.now() + 60000;
  const wait = () => {
    if (fs.existsSync(sentinel) || Date.now() > deadline) return;
    setTimeout(wait, 100);
  };
  wait();
}
