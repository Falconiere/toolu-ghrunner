const fs = require('node:fs');
module.exports = function record(stage) {
  const e = process.env;
  const row = [stage, e.GITHUB_ACTION, e.GITHUB_ACTION_REPOSITORY,
    e.GITHUB_ACTION_REF, e.RUNNER_ENVIRONMENT].join('|');
  fs.appendFileSync('metadata.log', row + '\n');
};
