// Issue #89: detach a child that outlives the post stage, then optionally fail.
const { spawn } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');

console.log(`TRACK89|node-post|${process.env.RUNNER_TRACKING_ID ?? 'unset'}`);
const dir = process.env.ORPHAN89_PIDS;
if (dir) {
  // The detached child is this Node itself, not `sleep`: with SIP on, macOS
  // hides the environment of a setsid'd Apple platform binary such as
  // /bin/sleep, which no inheritance-based sweep can then find.
  const child = spawn(process.execPath, ['-e', 'setTimeout(() => {}, 305000)'], {
    detached: true,
    stdio: 'ignore',
  });
  fs.writeFileSync(path.join(dir, 'node-post'), String(child.pid));
  child.unref();
  if (process.env.ORPHAN89_POST_UNTAGGED === '1') {
    // Stays in this post's process group but without the id: only the
    // group match can find it.
    const env = { ...process.env };
    delete env.RUNNER_TRACKING_ID;
    const untagged = spawn(process.execPath, ['-e', 'setTimeout(() => {}, 310000)'], {
      env,
      stdio: 'ignore',
    });
    fs.writeFileSync(path.join(dir, 'node-untagged'), String(untagged.pid));
    untagged.unref();
  }
}
process.exitCode = process.env.ORPHAN89_POST_EXIT === '1' ? 1 : 0;
