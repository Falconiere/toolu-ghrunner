const fs = require('fs');
const path = require('path');

const marker = process.env.INPUT_MARKER || 'A';
const file = path.join(process.env.GITHUB_WORKSPACE, 'post-markers.txt');
fs.appendFileSync(file, `${marker}:main\n`);
console.log(`::save-state name=k::${marker}-state`);
if (process.env['INPUT_REMOVE-POST'] === 'true') {
  fs.unlinkSync(path.join(__dirname, 'post.js'));
}
if (marker === 'MAINFAIL') {
  process.exitCode = 1;
}
