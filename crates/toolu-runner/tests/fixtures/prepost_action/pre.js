// Pre stage: record that pre ran, in this step's marker prefix.
const fs = require('fs');
const marker = process.env.INPUT_MARKER || 'X';
const file = process.env.MARKER_FILE;
if (file) {
  fs.appendFileSync(file, `${marker}:pre\n`);
}
