const started = Math.floor(Date.now() / 1000);
console.log(`pre-start=${started}`);
setTimeout(() => {
  console.log(`pre-end=${Math.floor(Date.now() / 1000)}`);
}, 40000);
