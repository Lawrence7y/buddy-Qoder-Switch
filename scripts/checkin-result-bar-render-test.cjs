const assert = require('node:assert/strict');
const path = require('node:path');
const esbuild = require('esbuild');

(async () => {
  const root = path.join(__dirname, '..');
  const result = await esbuild.build({
    stdin: {
      contents: `
        import { createElement } from 'react';
        import { renderToStaticMarkup } from 'react-dom/server';
        import { CheckinResultBar } from './src/components/account-list-parts';
        export const pending = renderToStaticMarkup(createElement(CheckinResultBar, {
          okCount: 0, failedNames: [], pendingNames: ['A', 'B', 'C', 'D'],
          onRetry() {}, onDismiss() {},
        }));
        export const mixed = renderToStaticMarkup(createElement(CheckinResultBar, {
          okCount: 1, failedNames: ['Failed'], pendingNames: ['Waiting'],
          onRetry() {}, onDismiss() {},
        }));
      `,
      resolveDir: root, loader: 'ts',
    },
    bundle: true, write: false, platform: 'node', format: 'cjs',
    plugins: [{ name: 'render-fixture-boundaries', setup(build) {
      build.onResolve({ filter: /^@\/lib\/i18n$/ }, () => ({ path: 'i18n', namespace: 'fixture' }));
      build.onResolve({ filter: /^@\/components\/account-card$/ }, () => ({ path: 'avatar', namespace: 'fixture' }));
      build.onLoad({ filter: /.*/, namespace: 'fixture' }, ({ path: id }) => ({
        contents: id === 'avatar'
          ? `export const AVATAR_TEXT_CLASS=''; export const accountInitials=()=>''; export const avatarTone=()=>'';`
          : `import { zh } from ${JSON.stringify(path.join(root, 'src/locales/domains/shared.zh.ts'))};
             export const useT=()=> (key, args={}) => (zh[key] ?? key).replace(/\\{(\\w+)\\}/g, (_, k)=>String(args[k] ?? ''));`,
        loader: 'ts', resolveDir: root,
      }));
    } }],
  });
  const mod = { exports: {} };
  new Function('require', 'module', 'exports', result.outputFiles[0].text)(require, mod, mod.exports);
  assert.match(mod.exports.pending, /已确认 0 · 奖励待确认 4 · 失败 0/);
  assert.doesNotMatch(mod.exports.pending, /重试失败项|失败账号/);
  assert.match(mod.exports.pending, /待确认账号/);
  assert.match(mod.exports.mixed, /已确认 1 · 奖励待确认 1 · 失败 1/);
  assert.match(mod.exports.mixed, /重试失败项/);
  console.log('checkin result bar: pending-only and mixed DOM rendering passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
