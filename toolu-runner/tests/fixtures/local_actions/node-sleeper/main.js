// Local node action main: signal start, then stay alive long enough that only
// a kill (job cancel) can end the process before the done marker is written.
const fs = require('fs');
const file = process.env.MARKER_FILE;
if (file) {
  fs.appendFileSync(file, 'sleeper-start\n');
}
setTimeout(() => {
  if (file) {
    fs.appendFileSync(file, 'sleeper-done\n');
  }
}, 30000);
