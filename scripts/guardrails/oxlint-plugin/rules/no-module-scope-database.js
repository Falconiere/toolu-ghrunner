// no-module-scope-database — build the database handle per request, never once.
//
// On workerd a module is evaluated once per isolate, before any request, and
// that isolate is shared by every request the isolate serves. A `const db =
// createDatabase(...)` at module scope therefore captures whatever config
// existed at startup and hands the same handle to every caller — which is a
// connection-pool bug wearing a different hat, and the failure only shows up
// under concurrency, in production.
//
// Contextual rather than textual: the exact same call one line further in, in a
// function body, is the correct form. That is the distinction a grep cannot
// make, and it is why this is a rule rather than a banned string.
import { isInTestDir, isUnderSrc, relative } from '../config.js';

// `createDatabase` and `drizzle` are unambiguous. `createClient` is not — it is
// the constructor name for supabase, redis, urql and a dozen others, and
// flagging every module-scope `createClient()` would make this rule noise in
// files that have nothing to do with a database. So it counts only when this
// file imported it from a libsql/turso module.
const ALWAYS = new Set(['createDatabase', 'drizzle']);
const WHEN_IMPORTED_FROM_DATABASE = new Set(['createClient']);
const DATABASE_MODULE = /^(@libsql\/|@tursodatabase\/|drizzle-orm)/;

/** Is this node evaluated when the module loads, or only when something calls it? */
function isModuleScope(node) {
  for (let parent = node.parent; parent; parent = parent.parent) {
    switch (parent.type) {
      case 'FunctionDeclaration':
      case 'FunctionExpression':
      case 'ArrowFunctionExpression':
        return false;
      // Deliberately NOT ClassBody or MethodDefinition. A method's body is a
      // FunctionExpression and is already caught above, so listing them here
      // only ever exempted the thing that is not a method: a class FIELD
      // initializer, which runs per construction — and a `static` one runs at
      // module load, exactly the case this rule exists to catch.
      default:
        break;
    }
  }
  return true;
}

export const noModuleScopeDatabase = {
  meta: {
    type: 'problem',
    docs: { description: 'construct the database handle per request, not at module scope' },
  },
  create(context) {
    const relPath = relative(context.filename ?? context.getFilename?.() ?? '');
    if (!isUnderSrc(relPath) || isInTestDir(relPath)) return {};

    const fromDatabaseModule = new Set();

    return {
      ImportDeclaration(node) {
        if (!DATABASE_MODULE.test(node.source?.value ?? '')) return;
        for (const spec of node.specifiers ?? []) {
          if (spec.local?.name) fromDatabaseModule.add(spec.local.name);
        }
      },
      CallExpression(node) {
        if (node.callee?.type !== 'Identifier') return;
        const name = node.callee.name;
        const watched =
          ALWAYS.has(name) ||
          (WHEN_IMPORTED_FROM_DATABASE.has(name) && fromDatabaseModule.has(name));
        if (!watched) return;
        if (!isModuleScope(node)) return;
        context.report({
          node,
          message:
            `no-module-scope-database: ${node.callee.name}() at module scope — a Worker ` +
            'evaluates a module once per isolate and shares that isolate across requests, ' +
            'so this handle outlives the request that made it; move the call inside the ' +
            'function that needs it and pass the config in',
        });
      },
    };
  },
};
