const fs = require('fs');
const path = require('path');

const marker = process.env.INPUT_MARKER || 'A';
const file = path.join(process.env.GITHUB_WORKSPACE, 'post-markers.txt');
fs.appendFileSync(file, `${marker}:post:STATE_k=${process.env.STATE_k || '<unset>'}:INPUT_MARKER=${marker}:GITHUB_ACTION=${process.env.GITHUB_ACTION || '<unset>'}\n`);
const finish = () => {
  if (process.env['INPUT_FAIL-POST'] === 'true') {
    process.exitCode = 1;
  }
};
const sleepMs = Number(process.env['INPUT_SLEEP-MS'] || '0');
if (sleepMs > 0) {
  fs.appendFileSync(file, `${marker}:post-start\n`);
  console.log(`POST_STARTED:${marker}`);
  setTimeout(() => {
    fs.appendFileSync(file, `${marker}:post-finished\n`);
    finish();
  }, sleepMs);
} else {
  finish();
}
