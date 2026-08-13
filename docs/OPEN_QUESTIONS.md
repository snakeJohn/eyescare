# EyesCare Open Questions 与实现期歧义记录

> 规则（用户明确要求）：有产品歧义先记在这里，不擅自扩大到 Focus/Magic/云同步。

## 设计文档遗留 OQ（r2 修订后仍开放）

| # | 问题 | 状态 |
|---|------|------|
| 1 | 商标是否最终锁定 EyesCare？ | 开放 |
| 2 | Pro 价格与授权（买断 vs 订阅） | 开放（功能边界已有暂行表 KD17） |
| 3 | 内置规则模板完整国内 App 清单（微信/飞书/PR/AE…） | 开放（附录 B 仅示例） |
| 4 | 洞察文案语气：数据向 vs 关怀向 | 开放（当前默认数据向） |
| 5 | 家长控制是否仅 Pro | 开放（暂行：仅 Pro） |
| 6 | 与 Night Light 共存最终文案 A/B | 开放 |
| 7–11 | 已关闭（KD16/KD20/KD18/KD21/KD17） | 关闭 |

## 实现期新增歧义（2026-08-11 开发中记录）

| # | 歧义 | 默认决策（可回改） | 影响 |
|---|------|-------------------|------|
| I1 | DayNight 白天色温基准：`day_kelvin()` 用 5500（办公）而非 health 预设 4500 | 5500（更接近自然光认知） | display/day_night.rs |
| I2 | 空闲暂停阈值默认 240s（设计范围 3–5min 取 4min） | 240s | timer.rs |
| I3 | 「智能」预设 = DayNight 当前值，但 DayNight 关闭时回落到 health 4500 | 回落 health | resolver 默认 claim |
| I4 | Windows 自启命令：MVP 占位 `EyesCare.exe --autostart`，真实 exe 路径待壳层注入 | 占位 + TODO | platform-win/system.rs |
| I5 | 系统事件 MVP 用轮询（拓扑指纹 2s）替代消息窗口；SessionUnlocked/PowerResumed 暂不投递 | 轮询 + 文档标注 | platform-win/system.rs |
| I6 | HDR 检测用 DISPLAYCONFIG advanced color（Win10 1703+），失败返回 false 由用户设置兜底 | 尽力 + 用户设置 | platform-win/display.rs |
| I7 | GuidedBreak 音频资源（CC0 声景 ≤1.5MB）尚未选型，先留 assets/guided/ 目录占位 | 无音频（静默模式可用） | assets/guided/ |
| I8 | 托盘「今日遵从率」菜单项 MVP 仅打开洞察 Tab（无内嵌数据） | 打开设置窗洞察 Tab | 壳层 |
| I9 | config.json 存储目录：设计 §9.1 为 `%APPDATA%\EyesCare\`，实现拆 `config/` 子目录 | `%APPDATA%/EyesCare/config/*.json` + `insights.db` 同级 | ConfigStore |
| I10 | 规则持久化的 enabled 状态 | **已解决（2026-08-12）**：`Rule.enabled: Option<bool>`（serde skip_if_none），用户开关落盘；`effective_enabled()` 回落出厂值 | rules.rs |

| I11 | Windows 长路径（>260 字符）下前台进程名采集失败 → 场景无身份，规则不匹配（安全侧可接受） | 保持 MVP 简化；真机观测后决定是否加 32K buffer 重试 | platform-win/foreground.rs |
| I12 | EventBus（core/events.rs）已实现并测试，但壳层目前用 app.emit 直接推送前端；EventBus 待壳层接线 | 保留（测试覆盖），v0.2 事件驱动化时接入 | core/events.rs |
| I13 | 托盘「预设」菜单项为静态文本，不随预设切换更新 | MVP 保持；真机体验后决定 | 壳层 |

## 明确不做的（防范围膨胀）

- Focus / Magic Window / 自动暗黑（v0.3）
- 番茄钟、强制休息+密码、CLI、Profile 包（v0.2）
- 云同步、遥测（隐私默认本地）
- 日历联动、Chronotype（v0.2）
- Deep link（MVP-B）
- 摄像头、屏幕录制、Accessibility（MVP 不需要）
