// Main stage: save state for the post stage and record that main ran.
const fs = require('fs');
const marker = process.env.INPUT_MARKER || 'X';
const file = process.env.MARKER_FILE;

// save-state surfaces as STATE_k to THIS step's post stage.
console.log(`::save-state name=k::${marker}-state`);

if (file) {
  fs.appendFileSync(file, `${marker}:main\n`);
}
