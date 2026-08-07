// folder-tree — the allowed source tree shape, enforced by the linter.
//
// oxlint sees every source file's path, which is all this rule needs, so the
// folder tree stops being something a separate bash pass has to walk. Two rules
// from one config block:
//
//   src.topLevel   the only directories permitted directly under srcRoot
//   src.nested     a glob-keyed allowlist of subdirectories — a key ending /*
//                  governs every direct child, which is how intra-domain shape
//                  is expressed without naming any domain
//
// src.topLevel OMITTED means unconstrained. An empty array means nothing is
// allowed. They are different, and only the bash module ever sees the omitted
// case (the Rust stack, which oxlint cannot parse at all).
import { config, relative } from '../config.js';

/** The allowlist governing this directory's children, or null when unconstrained. */
function allowFor(parent) {
  const nested = config.src?.nested ?? {};
  for (const [key, values] of Object.entries(nested)) {
    if (key === parent) return values;
    if (key === '*') return values;
    if (key.endsWith('/*')) {
      const prefix = key.slice(0, -2);
      if (parent.startsWith(`${prefix}/`)) {
        const rest = parent.slice(prefix.length + 1);
        if (!rest.includes('/')) return values;
      }
    }
  }
  return null;
}

export const folderTree = {
  meta: {
    type: 'problem',
    docs: { description: 'source files live in a sanctioned directory' },
  },
  create(context) {
    const srcRoot = config.srcRoot;
    const relPath = relative(context.filename ?? context.getFilename?.() ?? '');

    return {
      Program(node) {
        if (!relPath.startsWith(`${srcRoot}/`)) return;

        const dir = relPath.slice(0, relPath.lastIndexOf('/'));
        const rel = dir.slice(srcRoot.length + 1);
        if (!rel) return;

        const segments = rel.split('/');
        const topLevel = config.src?.topLevel;
        if (topLevel && !topLevel.includes(segments[0])) {
          context.report({
            node,
            message:
              `folder-tree: "${segments[0]}" is not an allowed top-level directory under ` +
              `${srcRoot} — move it under one of: ${topLevel.join(', ')}`,
          });
          return;
        }

        // Walk every directory this file sits below; one new file can introduce
        // several at once, and the deepest is not always the wrong one.
        for (let depth = 1; depth < segments.length; depth += 1) {
          const parent = segments.slice(0, depth).join('/');
          const leaf = segments[depth];
          const allowed = allowFor(parent);
          // The configured test directory is allowed inside ANY constrained
          // directory, without every config listing it. CORE rule 6 requires a
          // test to sit in a sibling test dir beside the code it covers, so an
          // allowlist that omitted it would forbid what another house rule
          // mandates — console's src/api/ hit exactly that.
          if (leaf === config.testDir) continue;
          if (allowed && !allowed.includes(leaf)) {
            context.report({
              node,
              message:
                `folder-tree: "${leaf}" is not an allowed directory inside ${srcRoot}/${parent} ` +
                `— use one of: ${allowed.join(', ')}`,
            });
            return;
          }
        }
      },
    };
  },
};
