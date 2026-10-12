const fs = require('node:fs');
fs.appendFileSync(process.env.GITHUB_OUTPUT, 'ok=1\n');
fs.appendFileSync(process.env.GITHUB_STATE, 'saved<<EOF\nv\n');
