// B2 路由懒加载验收（webui-cdp-verify 场景）
//
// 断言的是**用户能看见的性质**：
//   1. 默认路由 `/` 真的渲染出来了（懒加载没有把首页搞崩）；
//   2. 首屏**没有**加载统计页才需要的 chunk（这是 B2 的全部收益所在）；
//   3. 点侧栏切到统计页后，那些 chunk **才**被加载。
//
// ★ 反面对照（第 2 条）是关键：只断言「统计页能打开」无法证明懒加载生效——
//   打包成一个大 chunk 时它同样能打开。
//
// ★ 同步陷阱：点侧栏后 `location.pathname` **同步**就变了，而懒加载 chunk 是**异步**拉的。
//   若在 `waitFor(pathname)` 之后立刻读 `performance` 资源表，会看到「新 chunk 还没到」——
//   2026-09-28 首跑就踩到，表现为「新增的 chunk 落后一次导航」。因此每一步都用
//   `waitFor(chunk 已加载)` 做同步点，再断言。
//
// ⚠️ 本场景只验证**产物切分与路由渲染**，不验证数据正确性：预览服务器后面没有后端，
// 接口会 `ERR_CONNECTION_REFUSED`（预期内），页面会落到错误/空态。
// 断言一律锚「路由 + chunk 加载集合 + #root 非空」，不锚接口返回的数据。

export default async function (ctx) {
  const assetsExpr =
    `performance.getEntriesByType("resource").map((r) => r.name.split("/").pop()).filter((n) => n.endsWith(".js"))`;
  const loaded = () => ctx.evaluate(assetsExpr);
  const has = (list, prefix) => list.some((n) => n.startsWith(prefix));

  // ---- 1. 默认路由渲染 ----
  await ctx.waitFor("侧栏出现", `!!document.querySelector('nav a[href="/"]')`, 20000);
  await ctx.waitFor("首页有内容", `document.querySelector("#root").textContent.length > 20`, 20000);

  ctx.check(
    "#root 已渲染（非白屏）",
    (await ctx.evaluate(`document.querySelector("#root").children.length`)) > 0,
  );

  const onHome = await loaded();
  ctx.check("首页已加载 AccountsPage 懒加载 chunk", has(onHome, "AccountsPage-"), onHome.join(" "));
  ctx.check(
    "★ 首屏未加载统计页 chunk（懒加载生效）",
    !has(onHome, "TokenStatsPage-") && !has(onHome, "CreditStatsPage-") && !has(onHome, "chart-"),
    onHome.join(" "),
  );

  // ---- 2. 切到 Token 统计页 ----
  await ctx.press(`document.querySelector('nav a[href="/token-stats"]')`);
  await ctx.waitFor("TokenStatsPage chunk 已加载", `${assetsExpr}.some((n) => n.startsWith("TokenStatsPage-"))`, 25000);
  ctx.check("Token 统计页路由正确", (await ctx.evaluate(`location.pathname`)) === "/token-stats");
  ctx.check(
    "Token 统计页渲染出实质内容",
    (await ctx.evaluate(`document.querySelector("#root").textContent.length`)) > 100,
  );
  const afterToken = await loaded();
  ctx.check(
    "★ 首页阶段确实没有 TokenStatsPage chunk（前置对照）",
    !has(onHome, "TokenStatsPage-"),
  );
  // ⚠️ `chart-*.js`（recharts 封装）由 **TokenStatsPage 静态引入**，因此在这一步就已加载。
  // 首跑时我误以为它属于积分页、把「切页前未加载」写成断言 ⇒ 假失败。
  // 判据：`grep 'from"./chart-' dist/assets/TokenStatsPage-*.js` 有命中。**是脚本错，不是应用错。**
  ctx.check(
    "★ 切页后才加载 recharts 主 chunk",
    has(afterToken, "chart-"),
    afterToken.filter((n) => n.startsWith("chart-")).join(" ") || "（未加载）",
  );

  // ---- 3. 切到积分统计页（只属于这一页的 chunk）----
  ctx.check("切页前 CreditStatsPage chunk 未加载", !has(afterToken, "CreditStatsPage-"));
  await ctx.press(`document.querySelector('nav a[href="/credit-stats"]')`);
  await ctx.waitFor("CreditStatsPage chunk 已加载", `${assetsExpr}.some((n) => n.startsWith("CreditStatsPage-"))`, 25000);
  ctx.check("积分统计页路由正确", (await ctx.evaluate(`location.pathname`)) === "/credit-stats");
  ctx.check(
    "积分统计页渲染出实质内容",
    (await ctx.evaluate(`document.querySelector("#root").textContent.length`)) > 100,
  );

  // ---- 4. 回首页 ----
  await ctx.press(`document.querySelector('nav a[href="/"]')`);
  await ctx.waitFor("回到首页", `location.pathname === "/"`, 15000);
  ctx.check(
    "返回首页正常（懒加载 chunk 已缓存，未崩）",
    (await ctx.evaluate(`document.querySelector("#root").children.length`)) > 0,
  );

  await ctx.screenshot("b2-lazy-routes.png");
}
