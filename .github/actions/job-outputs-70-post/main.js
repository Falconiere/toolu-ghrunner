const fs = require('fs');

fs.appendFileSync(process.env.GITHUB_STATE, 'marker=issue-70-main\n');
