const fs = require('node:fs');
fs.appendFileSync(process.env.GITHUB_STATE, `who=${process.env.INPUT_WHO}\n`);
