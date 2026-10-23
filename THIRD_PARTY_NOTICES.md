# 第三方来源

本目录基于 NextAgentX/qoder-trae-workbuddy-switch，基线提交
`cbd2bf9380a90a8743c5de00bc7437c593961e76`。
原始 `LICENSE` 中的 PolyForm Noncommercial 1.0.0 和 Required Notice 保留。

领奖励请求形态与国际版网页活跃协议参考 smart-open/TraeWorkAssistant：
`e44abbcf42038e9105111011ade689477ec8652f`，下列 MIT 许可适用于参考实现中移植、改编的部分。
其 Tauri AppState、同步 ureq 网关与账号存储没有搬入本程序。

用户附件 `0001-checkin-and-dsml-fix.patch` 中的 WorkBuddy 国内请求层、
Qoder DSML 恢复和结构化工具历史改动经过审阅后纳入，并作进一步修改。
附件不是独立的上游发布版，其通过测试和真机可用性由本项目重新核实。

## TraeWorkAssistant MIT License

Copyright (c) 2026 朱天伟

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
