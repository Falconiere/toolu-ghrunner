// Local node action main: record that it ran (with its INPUT_MARKER value).
const fs = require('fs');
const marker = process.env.INPUT_MARKER || 'node-leaf-main';
const file = process.env.MARKER_FILE;
if (file) {
  fs.appendFileSync(file, `${marker}\n`);
}
