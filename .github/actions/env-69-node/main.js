const fs = require('fs');
const path = require('path');

const marker = process.env.INPUT_MARKER;

if (!process.env.SECRET_VALUE || process.env.SECRET_VALUE !== process.env['INPUT_EXPECTED-SECRET']) {
  throw new Error('secret-bearing environment value mismatch');
}
process.stdout.write(`env69-node-secret:${process.env.SECRET_VALUE}\n`);

const expected = {
  shared: process.env['INPUT_EXPECTED-SHARED'],
  empty: process.env['INPUT_EXPECTED-EMPTY'],
  multiline: process.env['INPUT_EXPECTED-MULTILINE'],
  literal: process.env['INPUT_EXPECTED-LITERAL'],
};
if (process.env.SHARED !== expected.shared || process.env.EMPTY !== expected.empty ||
    process.env.MULTILINE !== expected.multiline || process.env.LITERAL !== expected.literal) {
  throw new Error(`environment mismatch for ${marker}`);
}
fs.appendFileSync(
  path.join(process.env.GITHUB_WORKSPACE, 'env-69-node.jsonl'),
  `${JSON.stringify({ marker, phase: 'main', shared: process.env.SHARED, empty: process.env.EMPTY, multiline: process.env.MULTILINE, literal: process.env.LITERAL, actionFile: process.env.ACTION_FILE || '' })}\n`,
);
fs.appendFileSync(process.env.GITHUB_STATE, `main=${marker}\n`);
fs.appendFileSync(process.env.GITHUB_ENV, `ACTION_FILE=from-${marker}\n`);
