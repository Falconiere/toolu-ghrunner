const fs = require('fs');
const path = require('path');

const marker = process.env.INPUT_MARKER;

if (!process.env.SECRET_VALUE || process.env.SECRET_VALUE !== process.env['INPUT_EXPECTED-SECRET']) {
  throw new Error('secret-bearing environment value mismatch');
}
process.stdout.write(`env69-node-secret:${process.env.SECRET_VALUE}\n`);

if (process.env.STATE_main !== marker) {
  throw new Error(`action state missing for ${marker}`);
}
fs.appendFileSync(
  path.join(process.env.GITHUB_WORKSPACE, 'env-69-node.jsonl'),
  `${JSON.stringify({ marker, phase: 'post', shared: process.env.SHARED, empty: process.env.EMPTY, multiline: process.env.MULTILINE, literal: process.env.LITERAL, actionFile: process.env.ACTION_FILE || '', main: process.env.STATE_main })}\n`,
);
