// #251: main sees pre's state (or, for a local use, that no pre ran) and
// records its own for post.
const fs = require('fs');
const tag = process.env.INPUT_TAG;
const wantPre = process.env['INPUT_EXPECT-PRE'] === 'false' ? undefined : tag;
if (process.env.STATE_pre !== wantPre) {
  console.log(`::error::compat-251 main ${tag}: pre state is ${process.env.STATE_pre}, wanted ${wantPre}`);
  process.exit(1);
}
fs.appendFileSync(process.env.GITHUB_STATE, `main=${tag}\n`);
console.log(`compat-251 main ${tag}`);
