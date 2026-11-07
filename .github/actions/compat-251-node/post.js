// #251: post sees pre (unless local) and main state, and posts run last-in,
// first-out: exactly the `posts-before` tags have already posted, in order.
const fs = require('fs');
const path = require('path');
const tag = process.env.INPUT_TAG;
const wantPre = process.env['INPUT_EXPECT-PRE'] === 'false' ? undefined : tag;
const record = path.join(process.env.RUNNER_TEMP, 'compat-251-posts.txt');
const seen = fs.existsSync(record) ? fs.readFileSync(record, 'utf8').split('\n').filter(Boolean).join('|') : '';
const want = process.env['INPUT_POSTS-BEFORE'] || '';
const problems = [];
if (process.env.STATE_pre !== wantPre) problems.push(`pre state ${process.env.STATE_pre}`);
if (process.env.STATE_main !== tag) problems.push(`main state ${process.env.STATE_main}`);
if (seen !== want) problems.push(`earlier posts ${JSON.stringify(seen)}, wanted ${JSON.stringify(want)}`);
fs.appendFileSync(record, `${tag}\n`);
if (problems.length) {
  console.log(`::error::compat-251 post ${tag}: ${problems.join('; ')}`);
  process.exit(1);
}
console.log(`compat-251 post ${tag}`);
