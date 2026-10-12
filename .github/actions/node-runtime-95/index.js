const fs = require('fs');
const header = process.report.getReport().header;
const observed = JSON.stringify({
  version: process.version,
  execPath: process.execPath,
  glibc: header.glibcVersionRuntime || null,
});
fs.writeFileSync('node-runtime-95.json', observed);
console.log(`NODE95|${observed}`);
