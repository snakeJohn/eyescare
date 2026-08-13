# EyesCare（护眼助手）

隐私优先、场景智能的跨平台护眼与节律助手。Windows 10/11 首发（Win-first dogfood），macOS 12+ 并行对齐（MVP-B）。

技术栈：Tauri 2 + Rust + React 18 + SQLite。设计文档见 `docs/design.md`（r2 修订版，上传副本存档于 `docs/`）。

## 仓库结构（阶段 1：少 crate，逻辑分模块）

```
apps/desktop/            # Tauri 2 + React 壳（托盘/自启/快捷键/Overlay/设置页）
crates/
  eyescare-platform/     # 平台 traits + 共享类型（DisplayBackend/ForegroundAppBackend/SystemBackend）
  eyescare-platform-win/ # Windows 后端（GDI Gamma、前台采样、系统事件）
  eyescare-platform-mac/ # macOS 后端（占位 stub，MVP-B 对齐）
  eyescare-core/         # 纯逻辑：config/display/safe_mode/scene/rules/timer/guided/insights
assets/guided/           # 引导休息资源（CC0，≤1.5MB）
docs/                    # 设计文档 + Open Questions + PR 计划
profiles/templates/      # 内置规则模板（JSON）
scripts/                 # 开发辅助脚本
```

## 差异化支柱（MVP-A 一等公民）

1. 场景智能（SceneEngine + RuleEngine + 内置模板）
2. 滤镜旁路 / 取色友好（SafeMode 状态机，P1 优先级）
3. 引导式休息（GuidedBreak，可静默）
4. 本地眼健康洞察（SQLite，90 天保留）
5. 配置 JSON 导入导出

## PR 状态

| PR | 内容 | 状态 |
|----|------|------|
| A01 | 仓库脚手架 + capabilities 最小集 | ✅ |
| A02 | 配置 schema v1 + 原子持久化 + 迁移 + 导入导出 | ✅ |
| A03 | 显示算法（kelvin/ramp/lerp/12Hz 限频） | ✅ |
| A04 | platform-win DisplayBackend | ✅（交叉 check 通过） |
| A06 | DisplayService + DayNight + Resolver P0–P5 + 紧急恢复 | ✅ |
| A07 | Shell（托盘/自启/快捷键/单实例） | ✅（真机构建待验证） |
| A08 | SafeModeController 状态机 | ✅ |
| A09/A10 | SceneContext + RuleEngine + 内置模板 | ✅ |
| A13/A14 | Timer 状态机 + 空闲暂停 + GuidedBreakPlayer | ✅ |
| A16 | Insights SQLite + 采集 + 今日面板 | ✅ |

冻结点：PR-A17（MVP-A ship gate）。

## 开发

```bash
# 纯逻辑（core/platform）可在任何平台编译测试：
cargo test -p eyescare-core
cargo test -p eyescare-platform

# Windows 后端交叉检查（在非 Windows 上）：
rustup target add x86_64-pc-windows-msvc
cargo check -p eyescare-platform-win --target x86_64-pc-windows-msvc

# 前端（apps/desktop）
cd apps/desktop && npm install && npm run tauri dev
```

## 隐私承诺

- 默认无网络、无遥测。
- 前台身份默认哈希落库；明文显示名可配置且默认关。
- 摄像头、屏幕录制、日历权限全部非 MVP。
