if (process.env.CI !== '' || process.env.GITHUB_ACTIONS !== 'true') {
  throw new Error(`node-post flags: CI=${process.env.CI}, GITHUB_ACTIONS=${process.env.GITHUB_ACTIONS}`);
}
console.log(`CI72|node-post|${process.env.CI}|${process.env.GITHUB_ACTIONS}|${require('node:os').hostname()}`);
