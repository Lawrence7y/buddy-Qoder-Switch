const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const ts = require('typescript');
const filename = path.join(__dirname, '../src/lib/checkin-results.ts');
const output = ts.transpileModule(fs.readFileSync(filename, 'utf8'), {
  compilerOptions: { module: ts.ModuleKind.CommonJS },
}).outputText;
const mod = { exports: {} };
new Function('module', 'exports', output)(mod, mod.exports);
const summarize = mod.exports.summarizeCheckinResults;
const pending = summarize([1, 2, 3, 4].map(id => ({ id: String(id), result: 'pending' })));
assert.equal(pending.ok, 0);
assert.equal(pending.already, 0);
assert.deepEqual(pending.failedIds, []);
assert.deepEqual(pending.pendingIds, ['1', '2', '3', '4']);
const mixed = summarize([
  { id: 'paid', result: 'success' }, { id: 'prior', result: 'already' },
  { id: 'waiting', result: 'pending' }, { id: 'failed', result: 'error' },
  { id: 'unknown', result: 'unexpected' },
]);
assert.deepEqual(mixed, { ok: 1, already: 1, pendingIds: ['waiting'], failedIds: ['failed', 'unknown'] });
assert.deepEqual(summarize([]), { ok: 0, already: 0, pendingIds: [], failedIds: [] });
console.log('checkin results: pending-only, mixed, unknown and empty cases passed');
