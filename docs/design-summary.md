# EyesCare 设计文档摘要（r2 评审修订后）

## 文档路径

| 文件 | 路径 |
|------|------|
| 设计文档 | `C:\Users\18888\AppData\Local\Temp\grok-18888\grok-design-doc-e947e77e.md` |
| 评审（已 addressed） | `C:\Users\18888\AppData\Local\Temp\grok-18888\grok-design-review-e947e77e.md` |
| 工作区 | `J:\AI项目\eyescare`（greenfield） |

**状态**：Draft r2 — 24 项评审均已 **addressed**；可按 MVP-A 关键路径开工。

## 叙事与支柱（保留）

**Parity 打底 + Differentiation 赢用户**

1. 场景智能（规则 + 全屏策略）  
2. **滤镜旁路 / 取色友好**（原安全模式；禁用「硬件级真彩」）  
3. 本地眼健康洞察  
4. 引导式休息  
5. 规则 / 配置可编程（完整 Profile/CLI 后置）

## MVP-A vs MVP-B

| | **MVP-A（可发布竖切）** | **MVP-B（≤2 周快跟）** |
|--|------------------------|------------------------|
| 显示 | Gamma 多屏 + 昼夜 + HDR skip；**Win-first** | 第二平台对齐 |
| 差异化 | 旁路状态机、简规则、引导短休、今日洞察、配置导出 | Deep link（运行中+确认+单实例）、Onboarding 60s |
| 计时 | 普通+20-20-20、**空闲暂停**、可跳过休息 | 打磨 |
| 冻结 PR | **PR-A17** | PR-B04 双端发布 |

## 关键架构默认（KD 节选）

- **栈**：Tauri 2 + Rust + React；先少 crate（core + platform-*）  
- **HDR**：跳过 gamma + 横幅（KD18）  
- **Win Gamma**：per-monitor `CreateDC` + readback + `WM_DISPLAYCHANGE` re-apply  
- **macOS**：wake/reconfig 后必须 re-apply  
- **Safe Mode**：`hold: while_match | duration`，防振荡 cooldown  
- **规则身份**：`process_name` / `bundle_id` + `equals|glob|…`  
- **游戏默认**：滤镜 pause + 休息 notify_only（exclusive 盖不住 overlay）  
- **洞察**：SQLite DDL；旁路时段 **不计入** 蓝光负荷  
- **Free**：旁路+基础规则+洞察；Pro：无限规则/强制密码/周报/CLI  
- **Deep link**：非 MVP-A  

## PR 结构

- **PR-A01…A18**：MVP-A（A07 壳 → A08 旁路；A06 含紧急恢复；freeze **A17**）  
- **PR-B01…B04**：MVP-B  
- **PR-P01…**：v0.2+ / post-MVP  

关键路径：Display 栈 → 旁路 → Rules → Break/Insights（约 8–13 人周 + 缓冲，1–2 人）。

## 相对 r1 的主要修复

平台失败模式与生命周期、Safe Mode 状态机、MVP 诚实切分、PR 依赖可合并、规则身份可实现、文案去夸大、权限/DDL/验收清单补齐。
