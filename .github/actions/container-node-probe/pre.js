const fs = require('fs');
const assert = require('assert');
assert.equal(process.platform, 'linux');
assert.equal(require('os').hostname(), 'container-73-probe');
fs.appendFileSync(process.env.GITHUB_STATE, 'from_pre=pre-value\n');
console.log('CONTAINER_73_NODE_PRE_OK');
