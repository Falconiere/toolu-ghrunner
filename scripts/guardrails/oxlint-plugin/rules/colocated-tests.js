// colocated-tests — a test lives beside the code it exercises.
//
// A test in a sibling __tests__/ is found by whoever opens the file it covers.
// A centralized test tree is found by nobody, and drifts. The linter sees the
// test file's own path, so it can say so at the moment the file is written.
//
// The bash module keeps the other half of this rule — a centralized `tests/`
// DIRECTORY existing at all — because that is a fact about the tree rather than
// about any file oxlint is asked to lint.
import { config, isInTestDir, isUnderSrc, relative } from '../config.js';

function isTestFile(relPath) {
  const base = relPath.slice(relPath.lastIndexOf('/') + 1);
  return (config.testGlob ?? '')
    .split(/\s+/)
    .filter(Boolean)
    .some((glob) => new RegExp(`^${glob.replaceAll('.', String.raw`\.`).replaceAll('*', '.*')}$`).test(base));
}

export const colocatedTests = {
  meta: {
    type: 'problem',
    docs: { description: 'tests colocate in a sibling test directory' },
  },
  create(context) {
    const relPath = relative(context.filename ?? context.getFilename?.() ?? '');
    return {
      Program(node) {
        if (!isUnderSrc(relPath) || !isTestFile(relPath) || isInTestDir(relPath)) return;
        context.report({
          node,
          message:
            `colocated-tests: test file outside a ${config.testDir}/ directory — ` +
            `move it to a sibling ${config.testDir}/ beside the file it covers`,
        });
      },
    };
  },
};
