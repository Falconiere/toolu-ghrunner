// #251: main sees pre's state and records its own for post.
const fs = require('fs');
const tag = process.env.INPUT_TAG;
if (process.env.STATE_pre !== tag) {
  console.log(`::error::compat-251 main ${tag}: pre state is ${process.env.STATE_pre}`);
  process.exit(1);
}
fs.appendFileSync(process.env.GITHUB_STATE, `main=${tag}\n`);
console.log(`compat-251 main ${tag}`);
