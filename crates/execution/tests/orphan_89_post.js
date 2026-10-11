// Issue #89: detach a child that outlives the post stage, then optionally fail.
const { spawn } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');

console.log(`TRACK89|node-post|${process.env.RUNNER_TRACKING_ID ?? 'unset'}`);
const dir = process.env.ORPHAN89_PIDS;
if (dir) {
  const child = spawn('sleep', ['305'], { detached: true, stdio: 'ignore' });
  fs.writeFileSync(path.join(dir, 'node-post'), String(child.pid));
  child.unref();
}
process.exitCode = process.env.ORPHAN89_POST_EXIT === '1' ? 1 : 0;
