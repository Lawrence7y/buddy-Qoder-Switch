import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import * as api from "@/lib/api";

/**
 * 「现在该不该跑周期性请求」——把分散在各处的可见性判断收敛成一个 hook。
 *
 * ## 为什么需要它
 *
 * 页面里的 `setInterval` 在应用切到后台、或桌面端窗口被隐藏/最小化后**仍会照跑**，
 * 于是持续打后端接口：用户看不见任何东西，代价却照付。
 *
 * 本 hook 只回答「现在该不该跑」，**刻意不改动调用方的轮询间隔** —— 那些间隔是产品决定
 * （例如 Trae 设置页「日志实时跟随」的 2 秒），性能优化没有权力单方面改它。
 *
 * ## 判据（= 既有两处实现的公共部分）
 *
 * | 来源 | 处理 |
 * |---|---|
 * | `document.visibilityState` | `hidden` ⇒ 不可见 |
 * | Tauri 桌面端 `main-window-visible` 事件 | 窗口隐藏 ⇒ 不可见（**桌面端必须有这一路**：窗口被最小化/隐藏时 WebView 的页面可见性不保证跟着变） |
 * | webui（浏览器） | 只用 `visibilityState`，不去监听不存在的 Tauri 事件 |
 *
 * 参照 `use-credit-auto-refresh.ts` 与 `use-workbuddy-status-refresh.ts`：两者各自
 * 实现了同一套判断（前者 `visibilitychange` + Tauri 事件，后者另加 `focus`/`blur`）。
 * 本 hook 取**两者公共的那部分**，因此可以安全地复用到任何轮询点；
 * `focus`/`blur` 是更强的策略（窗口可见但失焦也停），由需要它的调用方自行叠加。
 *
 * ## 为什么只门控「周期性请求」
 *
 * 首次挂载 / 路由切换时的加载**必须照常发生**，否则「切到后台再切回来」会看到空态。
 * 所以用法是只把本值放进**定时器**的判据里：
 *
 * ```tsx
 * const visible = useDocumentVisible();
 * useEffect(() => {
 *   if (!enabled || !visible) return;
 *   const timer = setInterval(tick, intervalMs);
 *   return () => clearInterval(timer);
 * }, [enabled, visible, intervalMs, tick]);
 * ```
 */
export function useDocumentVisible(): boolean {
  const [visible, setVisible] = useState<boolean>(() => readVisible());

  useEffect(() => {
    const onVisibility = () => setVisible(readVisible());
    document.addEventListener("visibilitychange", onVisibility);

    // 桌面端：窗口隐藏/显示由宿主事件告知。webui 下没有这条通道。
    let unlisten: (() => void) | undefined;
    if (!api.isWebui()) {
      void listen<boolean>("main-window-visible", (event) => {
        setVisible(event.payload);
      }).then((fn) => {
        unlisten = fn;
      });
    }

    return () => {
      document.removeEventListener("visibilitychange", onVisibility);
      unlisten?.();
    };
  }, []);

  return visible;
}

function readVisible(): boolean {
  if (typeof document === "undefined") return true;
  return document.visibilityState !== "hidden";
}
