// Post stage: must see STATE_k saved by THIS step's main stage.
const fs = require('fs');
const marker = process.env.INPUT_MARKER || 'X';
const file = process.env.MARKER_FILE;
const state = process.env.STATE_k || '<unset>';
if (file) {
  fs.appendFileSync(file, `${marker}:post:STATE_k=${state}\n`);
}
