// no-barrels — no re-export layer.
//
// With no barrel to hide behind, a grep for a symbol lands on its definition and
// an export nothing imports is genuinely dead, which is what makes knip accurate
// rather than merely enthusiastic. oxlint's no-restricted-imports already blocks
// IMPORTING through a barrel; this catches the barrel file existing at all.
//
// barrelExempt is per-stack and the difference is real: console exempts
// src/app/** because index.tsx is TanStack Router's name for a directory's "/"
// route, while expo exempts nothing — its router lives at app/ in the repo root,
// outside srcRoot, so every index.ts(x) under src/ is a true barrel.
import { config, isUnderSrc, matchesAny, relative } from '../config.js';

export const noBarrels = {
  meta: { type: 'problem', docs: { description: 'no barrel files' } },
  create(context) {
    const relPath = relative(context.filename ?? context.getFilename?.() ?? '');
    if (!isUnderSrc(relPath)) return {};
    const base = relPath.slice(relPath.lastIndexOf('/') + 1);
    if (!(config.barrelNames ?? []).includes(base)) return {};
    if (matchesAny(relPath, config.barrelExempt)) return {};

    return {
      Program(node) {
        context.report({
          node,
          message:
            'no-barrels: barrel file — delete it and import the concrete file; ' +
            'a re-export layer hides dead code from knip',
        });
      },
    };
  },
};
