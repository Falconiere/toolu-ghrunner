const fs = require('node:fs');
fs.appendFileSync(process.env.GITHUB_STATE, `who=${process.env.INPUT_WHO}\n`);
if (process.env.LOCAL_ONLY !== `node-${process.env.INPUT_WHO}`) process.exit(1);
