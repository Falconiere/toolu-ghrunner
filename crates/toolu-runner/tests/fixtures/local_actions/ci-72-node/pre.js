if (process.env.CI !== '' || process.env.GITHUB_ACTIONS !== 'true') {
  throw new Error(`node-pre flags: CI=${process.env.CI}, GITHUB_ACTIONS=${process.env.GITHUB_ACTIONS}`);
}
console.log(`CI72|node-pre|${process.env.CI}|${process.env.GITHUB_ACTIONS}|${require('node:os').hostname()}`);
