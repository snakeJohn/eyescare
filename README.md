# EyesCare

EyesCare 是一款隐私优先的桌面护眼与节律助手，当前面向 Windows 10/11。它在本地调节显示色温与亮度，并根据前台应用、全屏状态和用户习惯提供旁路、休息提醒与洞察。

## 功能

- 色温与亮度调节：提供预设、手动微调，以及按白天/夜间时间自动切换。
- 多显示器支持：支持同步模式和按显示器分别配置。
- HDR 兼容：默认跳过 HDR 显示器的 gamma 调节，也可由用户选择强制应用。
- 场景规则：基于进程名、应用标识、路径后缀与全屏状态匹配规则，自动切换预设、旁路或休息策略。
- 滤镜旁路：在取色、设计或其他色彩敏感场景下临时恢复原始显示。
- 休息提醒：支持普通节律与 20-20-20 模式、空闲暂停、静默提醒和引导式休息。
- 全局快捷键：可配置滤镜开关、旁路切换与立即开始引导休息的快捷键。
- 本地洞察：使用本地 SQLite 保存休息与使用情况摘要；默认不上传数据、不启用遥测。
- 配置管理：配置和规则可导入、导出，并以原子写入方式保存在本机。
- 托盘常驻：关闭设置窗口不会退出应用；从托盘菜单打开设置或退出应用。
- 深色与浅色主题：设置页支持在两种主题之间切换，并保留本地偏好。

## 本地开发

前置条件：

- Rust stable（Windows 开发建议安装 MSVC 构建工具）。
- Node.js 20 或更高版本。
- Windows 10/11：运行完整桌面应用所需；核心逻辑测试可在其他平台运行。

安装前端依赖并启动桌面开发环境：

```bash
cd apps/desktop
npm install
npm run tauri dev
```

构建前端：

```bash
cd apps/desktop
npm run build
```

运行核心逻辑测试：

```bash
cargo test -p eyescare-core
```

检查桌面端 Rust 集成：

```bash
cargo check -p eyescare-desktop
```

可选：检查 Windows 平台后端：

```bash
rustup target add x86_64-pc-windows-msvc
cargo check -p eyescare-platform-win --target x86_64-pc-windows-msvc
```

## 发布

在 GitHub 打开 **Actions → Release → Run workflow**，即可直接生成新的 GitHub Release：

- **version** 留空、**bump** 选 `patch`（默认）：按当前版本发下一个补丁版（例如 `0.1.0` → `v0.1.1`）
- **version** 填 `0.2.0` 或 `v0.2.0`：发布指定版本
- 勾选 **prerelease** / **draft** 可发预发布或草稿
- 版本号有变化时，工作流会自动改 `Cargo.toml` / `package.json` / `tauri.conf.json`、提交并打 tag

也可以本地打 tag 后推送（tag 必须与仓库当前版本一致）：

```bash
git tag v0.2.0
git push origin v0.2.0
```

产物为 Windows NSIS 安装包、免安装 ZIP 和 `SHA256SUMS.txt`。
