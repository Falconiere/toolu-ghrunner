const fs = require('fs');

if (process.env.STATE_marker !== 'issue-70-main') {
  throw new Error('post state was not restored');
}
fs.appendFileSync(process.env.GITHUB_ENV, 'POST_VALUE=after-post\n');
