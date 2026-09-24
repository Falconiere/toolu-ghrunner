const assert = require('node:assert/strict');
const fs = require('node:fs');
const values = {
  who: process.env.INPUT_WHO,
  expected: process.env.INPUT_EXPECTED,
  fallback: process.env.INPUT_FALLBACK,
};
assert.equal(values.who, values.expected);
assert.equal(values.fallback, values.expected);
fs.writeFileSync('node-inputs.json', JSON.stringify(values));
