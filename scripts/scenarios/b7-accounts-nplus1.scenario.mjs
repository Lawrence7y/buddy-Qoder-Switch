// B7 验收：webui 下「账号页」不得对每个账号各拉一次**整端点**。
//
// ## 为什么必须这样验
//
// 修复的效果是「N 次请求变 1 次」——这是**请求次数**，读代码看不出来、看界面也看不出来。
// 所以这里配一个会统计次数的 mock 后端（`scripts/mock-webui-backend.py`），直接断言次数。
//
// ## 反面对照
//
// 把 `AccountsPage.fetchTodayCheckinMap` 改回「对每个账号调一次 `api.getCheckinStatus`」，
// 本场景立刻变红（3 个账号 ⇒ 计数 3）。
//
// ## ★ 前置：mock 必须是**刚启动的**
//
// 计数在 mock 进程内累积。**不要**在场景里 `location.reload()` 来重置——
// 那会打断 CDP 的页面上下文，后续 `evaluate` 落在旧 context 上（实测：reload 后
// 卡片永远等不到）。正确做法是**每次跑之前重启 mock**，让计数天然从 0 开始：
//
//   python scripts/mock-webui-backend.py 57890 &   # 先杀掉旧的
//   node ~/.workbuddy-ai/skills/webui-cdp-verify/scripts/cdp-drive.mjs \
//        http://127.0.0.1:4177/ scripts/scenarios/b7-accounts-nplus1.scenario.mjs logs

const BACKEND = "http://127.0.0.1:57890";

export default async function (ctx) {
  await ctx.waitFor("侧栏出现", `!!document.querySelector('nav a[href="/"]')`, 20000);
  await ctx.waitFor(
    "账号卡片已渲染",
    `document.querySelector("#root").textContent.includes("测试账号 0")`,
    25000,
  );
  // 签到/旅行两批请求不阻塞卡片渲染，留出完成时间
  await ctx.sleep(2500);

  const counts = await ctx.evaluate(
    `fetch("${BACKEND}/__counts").then((r) => r.json()).then((c) => JSON.stringify(c))`,
  );
  const parsed = JSON.parse(counts);
  ctx.check("mock 后端收到过请求", Object.keys(parsed).length > 0, counts);

  const accounts = parsed["GET /api/accounts"] ?? 0;
  const checkin = parsed["GET /api/checkin/status"] ?? 0;
  const travel = parsed["GET /api/travel/status"] ?? 0;

  // mock 固定返回 3 个账号 ⇒ 旧实现下 checkin/travel 都会是 3。
  //
  // ⚠️ `/api/accounts` **不**按账号数增长：它每个 region 拉一次（CN + Global = 2）。
  // 首跑时我按 `=== 1` 断言 ⇒ 假失败 —— **是脚本错，不是应用错**（同一类错误本轮第二次）。
  ctx.check(
    "账号列表按 region 拉取（2 次），不随账号数增长",
    accounts === 2,
    `GET /api/accounts = ${accounts}（CN + Global 各一次；若变成 3/4 则说明又出现了按账号逐个拉）`,
  );
  ctx.check(
    "★ 签到状态只请求 1 次（不是每账号一次）",
    checkin === 1,
    `GET /api/checkin/status = ${checkin}（旧实现下应为 3）`,
  );
  ctx.check(
    "★ 旅行状态只请求 1 次（不是每账号一次）",
    travel === 1,
    `GET /api/travel/status = ${travel}（旧实现下应为 3）`,
  );

  await ctx.screenshot("b7-accounts-nplus1.png");
}
