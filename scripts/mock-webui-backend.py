#!/usr/bin/env python3
"""B7 验收用 mock 后端：为 webui 构建提供最小可用数据，并**统计每个端点的调用次数**。

为什么需要它：N+1 修复的效果是「N 次请求变 1 次」，这是一个**请求次数**断言，
靠读代码或看界面都证明不了。本 mock 记录次数，`/__counts` 供 CDP 场景读取。

用法：
    python scripts/mock-webui-backend.py [port]      # 缺省 57890

只监听 127.0.0.1；不是产品运行时组件，不要用于对外服务。
"""
import json
import sys
import threading
from collections import Counter
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 57890
ACCOUNT_COUNT = 3

COUNTS: Counter = Counter()
LOCK = threading.Lock()

ACCOUNTS = [
    {
        "id": f"acc-{i}",
        "uid": f"uid-{i}",
        "nickname": f"测试账号 {i}",
        "email": f"user{i}@example.com",
        "needsRelogin": False,
    }
    for i in range(ACCOUNT_COUNT)
]


def payload_for(path: str, query: str):
    if path == "/__counts":
        with LOCK:
            return dict(COUNTS)
    if path == "/api/status":
        return {
            "running": False,
            "region": "cn",
            "authFile": None,
            "current": None,
            "appPath": None,
            "version": "test",
            "installed": False,
            "regionMismatch": None,
        }
    if path == "/api/accounts":
        return {"region": "cn", "accounts": ACCOUNTS, "current": None}
    if path == "/api/checkin/status":
        return {
            "accounts": [
                {
                    "accountId": a["id"],
                    "email": a["email"],
                    "ok": True,
                    "todayCheckedIn": False,
                }
                for a in ACCOUNTS
            ]
        }
    if path == "/api/travel/status":
        return {
            "accounts": [
                {
                    "accountId": a["id"],
                    "email": a["email"],
                    "label": "untraveled",
                    "rewardCredit": None,
                }
                for a in ACCOUNTS
            ]
        }
    if path == "/api/travel/config":
        return {"enabled": True}
    if path == "/api/checkin/config":
        return {"enabled": True}
    if path == "/api/schedule/config":
        return {}
    if path == "/api/credits/stats":
        return {"ok": True, "accounts": [], "total": 0}
    return {}


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _respond(self, body: bytes):
        self.send_response(200)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        # 前端在 127.0.0.1:4177 上跑，这里是跨源 ⇒ 必须回 CORS 头
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Headers", "*")
        self.send_header("Access-Control-Allow-Methods", "GET,POST,OPTIONS")
        self.end_headers()
        self.wfile.write(body)

    def _handle(self):
        # ★ 必须把请求体读干净：HTTP/1.1 是 keep-alive，未读的 body 会被当成
        #   下一个请求行 ⇒ **协议失步** ⇒ 浏览器把后续请求报成 CORS 失败
        #   （症状：同样的端点时通时不通，且报「缺少 Access-Control-Allow-Origin」）。
        length = int(self.headers.get("Content-Length") or 0)
        if length > 0:
            self.rfile.read(length)

        parsed = urlparse(self.path)
        path = parsed.path
        with LOCK:
            COUNTS[f"{self.command} {path}"] += 1
        if self.command == "OPTIONS":
            self._respond(b"{}")
            return
        body = json.dumps(payload_for(path, parsed.query)).encode("utf-8")
        self._respond(body)

    do_GET = _handle
    do_POST = _handle
    do_OPTIONS = _handle

    def log_message(self, *args):  # 静音，避免刷屏
        pass


if __name__ == "__main__":
    server = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    print(f"mock webui backend on http://127.0.0.1:{PORT} ({ACCOUNT_COUNT} accounts)", flush=True)
    server.serve_forever()
