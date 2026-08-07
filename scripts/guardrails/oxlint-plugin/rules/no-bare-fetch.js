// no-bare-fetch — one configured HTTP client, not scattered fetch calls.
//
// Auth headers, base URL, timeouts and error shaping live in one place each.
// The rule is contextual rather than textual: the http client's own file is
// exempt, which is exactly the distinction a grep cannot make and the reason
// this was an ast-grep rule before oxlint could host it.
import { isInTestDir, isUnderSrc, matchesAny, relative } from '../config.js';

const EXEMPT = ['**/utilities/http.ts', '**/api/http-client.ts'];

export const noBareFetch = {
  meta: {
    type: 'problem',
    docs: { description: 'call the configured http client rather than bare fetch' },
  },
  create(context) {
    const relPath = relative(context.filename ?? context.getFilename?.() ?? '');
    if (!isUnderSrc(relPath) || matchesAny(relPath, EXEMPT) || isInTestDir(relPath)) return {};

    return {
      CallExpression(node) {
        if (node.callee?.type !== 'Identifier' || node.callee.name !== 'fetch') return;
        context.report({
          node,
          message:
            'no-bare-fetch: bare fetch() — call the configured http client instead, so auth ' +
            'headers, baseUrl, timeouts and error shaping stay in one place',
        });
      },
    };
  },
};
