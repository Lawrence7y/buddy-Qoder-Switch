#!/usr/bin/env node
/**
 * 门禁：zustand store hook 必须带 **selector**。
 *
 * ## 为什么需要它
 *
 * `useXxxStore()`（空参数）订阅的是**整个 store** —— 任何与页面无关的 state 变更都会
 * 触发该组件重渲染。本仓库其它地方都带了 selector，只有 `CreditStatsPage` 漏过一处：
 * 那是个 1900+ 行、内含多个 recharts 图表的页面，而 store 每 30 分钟刷新积分、
 * 每 60 秒刷新运行状态、且按账号逐个 `setState` ⇒ 每次无关变更都整页重渲染。
 *
 * 前端没有单测框架，所以用静态扫描把这条约定固定下来。
 *
 * ## 判据
 *
 * `use` + 标识符 + `Store` + **空括号**，例如 `useAccountsStore()`。
 *
 * **不误伤**：
 * - `useAccountsStore.getState()` —— 点号，不是调用（正则要求 `Store` 紧跟 `(`）；
 * - `useAccountsStore(useShallow(...))` / `useAccountsStore((s) => s.x)` —— 有参数；
 * - 注释与字符串字面量里的示例 —— 扫描前已剥离。
 *
 * 用法：`node scripts/check-store-selectors.cjs`（已接进 `npm run build`）。
 */

const fs = require("node:fs");
const path = require("node:path");

const ROOT = path.resolve(__dirname, "..");
const SRC = path.join(ROOT, "src");
const EXTS = new Set([".ts", ".tsx"]);

/**
 * 把注释与字符串字面量替换为**等长空白**，保持行列号不变。
 *
 * 逐字符重建（不是正则替换）是为了正确处理：字符串里的 `//`、模板字面量、
 * 跨行的块注释 —— 用正则会在这三处产生假阳性。
 */
function maskNonCode(source) {
  const chars = source.split("");
  const n = chars.length;
  let i = 0;
  const blank = (from, to) => {
    for (let k = from; k < to && k < n; k += 1) {
      if (chars[k] !== "\n") chars[k] = " ";
    }
  };
  while (i < n) {
    const c = chars[i];
    const next = chars[i + 1];
    if (c === "/" && next === "/") {
      const start = i;
      while (i < n && chars[i] !== "\n") i += 1;
      blank(start, i);
      continue;
    }
    if (c === "/" && next === "*") {
      const start = i;
      i += 2;
      while (i < n && !(chars[i] === "*" && chars[i + 1] === "/")) i += 1;
      i = Math.min(i + 2, n);
      blank(start, i);
      continue;
    }
    if (c === '"' || c === "'" || c === "`") {
      const quote = c;
      const start = i;
      i += 1;
      while (i < n) {
        if (chars[i] === "\\") {
          i += 2;
          continue;
        }
        if (chars[i] === quote) {
          i += 1;
          break;
        }
        i += 1;
      }
      blank(start, i);
      continue;
    }
    i += 1;
  }
  return chars.join("");
}

function walk(dir, out) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      walk(full, out);
    } else if (EXTS.has(path.extname(entry.name))) {
      out.push(full);
    }
  }
  return out;
}

/** 空参数 store hook 调用：`useXxxStore()`。 */
const EMPTY_STORE_HOOK = /\buse[A-Za-z0-9_]*Store\s*\(\s*\)/g;

function main() {
  if (!fs.existsSync(SRC)) {
    console.error(`[check-store-selectors] 找不到 src 目录：${SRC}`);
    process.exit(1);
  }

  const violations = [];
  for (const file of walk(SRC, [])) {
    const source = fs.readFileSync(file, "utf8");
    const masked = maskNonCode(source);
    const lines = masked.split("\n");
    const rawLines = source.split("\n");
    lines.forEach((line, index) => {
      EMPTY_STORE_HOOK.lastIndex = 0;
      const match = EMPTY_STORE_HOOK.exec(line);
      if (!match) return;
      violations.push({
        file: path.relative(ROOT, file).replace(/\\/g, "/"),
        line: index + 1,
        snippet: rawLines[index].trim(),
        call: match[0],
      });
    });
  }

  if (violations.length > 0) {
    console.error("[check-store-selectors] 以下位置订阅了整个 store（缺少 selector）：\n");
    for (const v of violations) {
      console.error(`  ${v.file}:${v.line}  ${v.call}`);
      console.error(`    ${v.snippet}`);
    }
    console.error(
      [
        "",
        "修法：用 selector 只订阅本组件用到的字段；多字段时套 `useShallow` 做浅比较：",
        "",
        "  import { useShallow } from \"zustand/react/shallow\";",
        "",
        "  const { a, b } = useAccountsStore(",
        "    useShallow((state) => ({ a: state.a, b: state.b })),",
        "  );",
        "",
        "为什么要拦：空参数订阅整个 store，任何无关 state 变更都会让该组件重渲染。",
      ].join("\n"),
    );
    process.exit(1);
  }

  console.log("[check-store-selectors] 通过：未发现空参数的 store hook 订阅。");
}

main();
