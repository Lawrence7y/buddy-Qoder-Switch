// 构建体积量化配置（B0）——**只用于量体积，不是产品构建**。
//
// 用法：`npx vite build --config scripts/bundle-report.config.ts`
// 产物写到 `dist-analyze/`（已被 `.gitignore` 的 `dist-*` 覆盖），量完删掉即可。
//
// 为什么需要它：首屏单 chunk 1.4 MiB 里各依赖占多少，靠读源码估不出来。
// 本配置把 `node_modules` 按包拆成独立 chunk，从而拿到 recharts / radix / react
// 的真实字节数，据此决定「要不要为懒加载引入复杂度」。
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import path from "node:path";

function pkgOf(id: string): string | null {
  const norm = id.replace(/\\/g, "/");
  const marker = "/node_modules/";
  const at = norm.lastIndexOf(marker);
  if (at === -1) return null;
  const rest = norm.slice(at + marker.length);
  const parts = rest.split("/");
  return rest.startsWith("@") ? `${parts[0]}/${parts[1]}` : parts[0];
}

export default defineConfig({
  base: "/",
  plugins: [react(), tailwindcss()],
  // 本文件在 `scripts/` 下，源码根在上一级。
  resolve: { alias: { "@": path.resolve(__dirname, "../src") } },
  build: {
    outDir: "dist-analyze",
    emptyOutDir: true,
    rollupOptions: {
      output: {
        manualChunks(id) {
          const pkg = pkgOf(id);
          if (!pkg) return undefined;
          // recharts + 它的传递依赖（d3-* / victory-vendor / lodash 等）算作一组，
          // 否则真实占比会被低估——它们只在图表页用得到。
          const rechartsFamily =
            pkg === "recharts" ||
            pkg.startsWith("d3-") ||
            pkg === "victory-vendor" ||
            pkg === "lodash" ||
            pkg === "react-is" ||
            pkg === "react-smooth" ||
            pkg === "recharts-scale" ||
            pkg === "tiny-invariant" ||
            pkg === "decimal.js-light" ||
            pkg === "eventemitter3";
          if (rechartsFamily) return "pkg-recharts";
          if (pkg.startsWith("@radix-ui/")) return "pkg-radix";
          if (pkg === "react" || pkg === "react-dom" || pkg === "scheduler") return "pkg-react";
          if (pkg === "lucide-react") return "pkg-lucide";
          if (pkg === "zustand" || pkg === "react-router-dom" || pkg === "react-router")
            return "pkg-app";
          return `pkg-other`;
        },
      },
    },
  },
});
