const assert = require('assert');
assert.equal(process.env.STATE_from_main, 'main-value');
assert.equal(process.env.STATE_from_pre, 'pre-value');
assert.equal(require('os').hostname(), 'container-73-probe');
require('fs').writeFileSync('post-marker', 'post-value');
console.log('CONTAINER_73_NODE_POST_OK');
