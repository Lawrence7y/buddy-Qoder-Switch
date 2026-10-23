// 图标缩图后的**视觉验收**（B7）。
//
// 只缩了源图像素（512 → 192 / 96），显示尺寸没变 ⇒ 唯一风险是**缩放后画质/透明通道出问题**。
// 15px 的原尺寸截图看不出质量，所以这里把 `<img>` 临时放大到 200px 再截图。
//
// 同时断言 `naturalWidth/Height`，证明页面上用的确实是新资源（而不是缓存里的旧图）。

export default async function (ctx) {
  await ctx.waitFor("侧栏出现", `!!document.querySelector('nav a[href="/"]')`, 20000);
  await ctx.waitFor("图标已挂载", `document.querySelectorAll('img').length > 0`, 20000);

  // 页面上所有 `<img>` 的自然尺寸（去重后）——证明新资源生效
  const naturals = await ctx.evaluate(
    `JSON.stringify([...new Set([...document.querySelectorAll("img")].map((i) => i.naturalWidth + "x" + i.naturalHeight))])`,
  );
  ctx.check("存在 192x192 的图标（workbuddy-official-icon 新尺寸）", naturals.includes("192x192"), naturals);
  ctx.check("不存在 512x512 的图标（旧尺寸已被替换）", !naturals.includes("512x512"), naturals);

  // 临时放大：把每个 img 的显示尺寸拉到 200px，方便肉眼判断缩放质量
  await ctx.evaluate(`(() => {
    for (const img of document.querySelectorAll("img")) {
      img.style.width = "200px";
      img.style.height = "200px";
      img.style.maxWidth = "none";
      img.style.objectFit = "contain";
      img.style.background = "#fff";
    }
    return true;
  })()`);
  await ctx.sleep(400);

  await ctx.screenshot("b7-icon-quality.png");
}
