const fs = require('fs');

const started = Math.floor(Date.now() / 1000);
console.log(`main-start=${started}`);
setTimeout(() => {
  const ended = Math.floor(Date.now() / 1000);
  console.log(`main-end=${ended}`);
  fs.appendFileSync(process.env.GITHUB_OUTPUT, `finished=${ended}\n`);
}, 40000);
