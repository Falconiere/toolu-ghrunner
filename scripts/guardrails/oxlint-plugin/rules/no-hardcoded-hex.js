// no-hardcoded-hex — colours come from the theme tokens.
//
// A hex literal outside the token files means a palette change is a search
// instead of one edit, and it is how light/dark drifts apart. The token files
// themselves are where the literals legitimately live, so they are exempt.
import { isInTestDir, isUnderSrc, matchesAny, relative } from '../config.js';

const EXEMPT = ['**/ui/theme/**', '**/theme/**'];
const HEX = /^#(?:[0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})$/i;

export const noHardcodedHex = {
  meta: {
    type: 'problem',
    docs: { description: 'colours come from ui/theme tokens, not hex literals' },
  },
  create(context) {
    const relPath = relative(context.filename ?? context.getFilename?.() ?? '');
    if (!isUnderSrc(relPath) || matchesAny(relPath, EXEMPT) || isInTestDir(relPath)) return {};

    return {
      Literal(node) {
        if (typeof node.value !== 'string' || !HEX.test(node.value)) return;
        context.report({
          node,
          message:
            `no-hardcoded-hex: hardcoded colour "${node.value}" — use a design token from ` +
            'ui/theme/ (palette.css on web, colors.ts on Expo) so a palette change stays one edit',
        });
      },
    };
  },
};
