# macOS 设计与开发计划文档任务

## 目标
基于 EyesCare 当前架构，编写可落地的 macOS 设计文档和逐步实施计划，在 `mac-doc` 分支提交并推送。

## 阶段
1. [complete] 盘点平台抽象、桌面集成、配置、测试和发布结构
2. [complete] 编写 macOS 架构与产品设计文档
3. [complete] 编写按测试驱动拆分的实施计划
4. [complete] 自审文档、检查 Git 差异并验证内容
5. [in_progress] 提交并推送 `mac-doc` 分支
6. [complete] 汇总子代理对 Windows 代码与免安装构建链路的审查结果

## 决策
- 文档必须区分可复用核心逻辑、macOS 平台实现和 Tauri 桌面集成。
- 实施计划使用精确路径、接口、测试命令和小步提交，避免占位内容。

## 遇到的错误
| 错误 | 尝试次数 | 解决方案 |
|---|---:|---|
| 首次并行检查因 `rg` 未找到 `AGENTS.md` 返回退出码 1，导致聚合调用失败 | 1 | 将“未找到”视为正常结果并显式输出 `NO_AGENTS_MD` |
| `dispatching-parallel-agents` 清单中的技能路径在本机不存在 | 1 | 保留并行边界，按 `review-spd` 模板直接调度子代理 |
| 首次创建审查子代理同时指定 full-history 与 agent type，被工具拒绝 | 1 | 保留 full-history，省略 agent type 后重试 |
