const fs = require('node:fs');
console.log('::add-mask::summary-83-dynamic');
fs.appendFileSync(process.env.GITHUB_STEP_SUMMARY, '# Node main\nsummary-83-dynamic\n');
