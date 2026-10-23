// 仓内管理层 UI SSR 渲染门禁：验证未观测状态与消耗事件负向符号
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const assert = require('node:assert');
const { createRequire } = require('node:module');

const appDir = path.resolve(__dirname, '..');
const requireApp = createRequire(path.join(appDir, 'package.json'));
const esbuild = requireApp('esbuild');

async function verify() {
  const reportsDir = process.env.MANAGEMENT_FIXTURES_DIR
    ? path.resolve(process.env.MANAGEMENT_FIXTURES_DIR)
    : path.resolve(appDir, 'fixtures/management');

  if (!fs.existsSync(reportsDir)) {
    assert.fail(`[check-management-ssr] 离线夹具目录不存在: ${reportsDir}。必须由 Rust 离线生成，缺夹具整体绝不能称通过！`);
  }

  const runId = Date.now().toString(36) + '-' + Math.random().toString(36).slice(2, 8);
  const tempDir = path.join(appDir, 'target', 'management-ssr-' + runId);
  fs.mkdirSync(tempDir, { recursive: true });

  const fixtures = ['empty', 'known'];
  let executedCount = 0;

  try {
    for (const name of fixtures) {
    const fixturePath = path.join(reportsDir, name + '.json');
    if (!fs.existsSync(fixturePath)) {
      assert.fail(`[check-management-ssr] 缺少必要离线夹具 ${fixturePath}，验证失败，整体绝不能称通过！`);
    }
    const fixture = JSON.parse(fs.readFileSync(fixturePath, 'utf8'));

    // 注意：被测数据必须是可重复生成的真实 Rust 序列化产物，禁止在验证前篡改数据以掩盖缺陷！
    executedCount++;

    const sourceFile = path.join(appDir, 'src/components/usage/credit-management-workbench.tsx');
    const source = fs.readFileSync(sourceFile, 'utf8');

    const dataPattern = /useState<CreditSummaryProjection\s*\|\s*null>\(null\)/;
    const loadingPattern = /(const\s*\[loading,\s*setLoading\]\s*=\s*useState\()true(\))/;
    assert(dataPattern.test(source), '源码中未找到 data 初始化模式');
    assert(loadingPattern.test(source), '源码中未找到 loading 初始化模式');

    const bundleFile = path.join(tempDir, `${name}-test-bundle.cjs`);
    await esbuild.build({
      stdin: {
        contents: `
          import React from "react";
          import { renderToStaticMarkup } from "react-dom/server";
          import { CreditManagementWorkbench } from "./src/components/usage/credit-management-workbench";
          export const render = () => renderToStaticMarkup(React.createElement(CreditManagementWorkbench, { scope: "wb-cn" }));
        `,
        resolveDir: appDir,
        loader: 'tsx',
      },
      outfile: bundleFile,
      bundle: true,
      platform: 'node',
      format: 'cjs',
      tsconfig: path.join(appDir, 'tsconfig.json'),
      logLevel: 'silent',
      define: {
        'import.meta.env': JSON.stringify({
          VITE_DEMO_MODE: '0',
          VITE_PAGES_DEMO: '0',
          DEV: false,
          PROD: true,
          MODE: 'test',
        }),
      },
      plugins: [
        {
          name: 'inject-test-state',
          setup(build) {
            build.onLoad({ filter: /credit-management-workbench\.tsx$/ }, () => ({
              contents: source
                .replace(dataPattern, `useState<CreditSummaryProjection | null>(${JSON.stringify(fixture)})`)
                .replace(loadingPattern, '$1false$2'),
              loader: 'tsx',
              resolveDir: path.dirname(sourceFile),
            }));
          },
        },
      ],
    });

    const { render } = require(bundleFile);
    const html = render();
    const text = html.replace(/<[^>]*>/g, ' ').replace(/\s+/g, ' ').trim();

    if (name === 'empty') {
      console.log('✓ 验证 empty 夹具 SSR 渲染结果...');
      // 1. 存储状态必须为未观测 (Unobserved)，绝不能误判为本地快照 (Cached)
      assert(text.includes('未观测 (Unobserved)'), 'empty 必须渲染未观测状态 badge');
      assert(!text.includes('本地快照 (Cached)'), 'empty 绝不能落入本地快照默认分支');

      // 2. 观测时间必须显示未观测，不能使用当前时间充当观测时间
      assert(text.includes('最近观测： 未观测'), 'empty 观测时间必须显示未观测');

      // 3. 统计卡片必须消费 isKnown=false，显示为 "—" 或 "未提供"，不能把未取到渲染成已确认 0
      assert(text.includes('当前可用余额 —'), '可用余额在未观测时必须显示为 —');
      assert(text.includes('总额度容量: 未提供'), '总额度容量在未观测时必须显示为 未提供');
      assert(text.includes('覆盖账号数 —'), '覆盖账号数在未观测时必须显示为 —');
      assert(text.includes('积分包总数 —'), '积分包总数在未观测时必须显示为 —');
      assert(text.includes('7天内即将到期 —'), '7天内即将到期在未观测时必须显示为 —');
      console.log('  [PASS] 未观测状态、空时间与未提供占位符断言全部通过');
    }

    if (name === 'known') {
      console.log('✓ 验证 known 夹具 SSR 渲染结果...');
      // 1. 消耗事件符号断言：usage 必须渲染为负向 (-1) 且带 rose 红色样式
      assert(html.includes('text-rose-600'), 'usage 消耗事件必须染成红色 (text-rose-600)');
      assert(text.includes('usage -1'), 'usage 消耗事件必须显示负号 (-1)');
      assert(!text.includes('usage +1'), 'usage 消耗事件绝不能显示为正向 (+1)');

      // 2. 正常状态与数值显示
      assert(text.includes('实时直连 (Live)'), 'known 正常显示 Live 状态');
      assert(text.includes('当前可用余额 15'), 'known 正常显示可用余额 15');
      console.log('  [PASS] usage 消耗事件负号与红色语义断言全部通过');
    }
    }
  } finally {
    try {
      fs.rmSync(tempDir, { recursive: true, force: true });
    } catch {
      // 忽略清理临时目录失败
    }
  }

  assert.strictEqual(executedCount, 2, '必须完整验证所有 2 个夹具，不允许跳过');
  console.log('\n[check-management-ssr] 所有管理层 SSR 验证用例全部通过！');
}

verify().catch((err) => {
  console.error('[check-management-ssr] 失败:', err);
  process.exit(1);
});
