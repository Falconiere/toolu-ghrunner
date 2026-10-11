// Issue #89: report the tracking id the Node main stage inherited.
console.log(`TRACK89|node-main|${process.env.RUNNER_TRACKING_ID ?? 'unset'}`);
