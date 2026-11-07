// #251: pre records its tag as state; main and post check it.
const fs = require('fs');
const tag = process.env.INPUT_TAG;
fs.appendFileSync(process.env.GITHUB_STATE, `pre=${tag}\n`);
console.log(`compat-251 pre ${tag}`);
