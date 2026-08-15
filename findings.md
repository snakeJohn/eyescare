# macOS 文档调查记录

- 项目为 Tauri 2 + React 前端、Rust 核心库与平台适配层。
- 当前分支为 `mac-doc`，工作树在任务恢复时干净。
- `fix/review-findings` 的本地和远程引用均已删除。
- 尚需从源码确认平台 trait、Windows 实现、Tauri 启动流程、权限、打包和 CI 的实际边界。
- Cargo workspace 已包含 `crates/eyescare-platform-mac`，说明现有分层预留了 macOS crate。
- `eyescare-desktop` 当前只在 `cfg(target_os = "windows")` 下依赖 Windows 平台 crate，没有 macOS 目标依赖。
- `tauri.conf.json` 的 bundle target 固定为 `nsis`，图标也只有 PNG/ICO，尚无 `.icns` 或 macOS bundle 配置。
- Release workflow 只有 `windows-latest` job，并将发布说明写死为 Windows 第一版。
- Tauri 已采用跨平台的通知、全局快捷键、开机启动和托盘插件，但其 macOS 权限与生命周期行为仍需在设计中明确。
- `DisplayBackend` 已覆盖枚举、apply/readback、逐屏/全部恢复、重绑定、HDR 探测和 HDR skip；macOS 不需要改 core 的显示服务契约。
- `ForegroundAppBackend` 的 `SceneContext` 已包含 `bundle_id` 和全屏置信度，RuleEngine 也已实现 bundle ID 大小写敏感匹配。
- `SystemBackend` 只要求空闲秒数、自启和电源/显示事件；桌面主循环依赖它暂停计时和在唤醒/拓扑变化后重新应用。
- 桌面启动当前把所有非 Windows 系统接到 Noop 后端；macOS 接入点位于 `apps/desktop/src-tauri/src/lib.rs` 的 setup 分支。
- 应用关闭设置窗时阻止退出，真正退出与 panic 路径均调用 `restore_all`；macOS 还需处理 Dock activation policy、terminate/sleep 生命周期。
- 当前 `Subscription` 是空结构，Drop 不会实际注销平台回调；macOS 设计必须决定由长期进程持有回调还是扩展为可注销句柄。
- 现有设计文档建议直接调用 `CGDisplayRestoreColorSyncSettings()`，但实现应优先逐屏恢复启动快照，避免覆盖其他软件在运行期间建立的全局 ColorSync 状态。
- macOS 基础前台身份可通过 `NSWorkspace.frontmostApplication` 获取，无需 Accessibility；焦点窗口尺寸/全屏判断应作为授权后的增强能力并明确降级。

## Windows 与免安装构建审查

- [High] `DisplayService::rebind_and_refresh()` 不会失效 current/target 缓存；系统唤醒或驱动重置 gamma 后，目标未变化会让 `recompute()` 跳过重新 apply。
- [High] Windows `restore_all()` 在首块 `CreateDC` 失败时提前结束，并忽略 `SetDeviceGammaRamp` 返回 false；后续屏可能退出后仍偏色。
- [Medium] 原始 ramp 快照读取失败时，恢复路径写入 identity，可能覆盖 EyesCare 从未成功读取或修改的 ICC/GPU 校准。
- [Medium] 滤镜关闭时显示变化事件被直接跳过；开启时重绑也没有重建 default/user claims，热插拔后的显示器 cache 和声明可能过期。
- [Medium] portable ZIP 只有裸 EXE，不包含 fixed WebView2 runtime；它是“免安装”而非离线自包含，在无 WebView2 的系统无法启动设置界面。
- 子代理验证：Windows Tauri NSIS 构建成功；`cargo test -p eyescare-platform-win -p eyescare-core` 共 130 项通过；旧 `v0.1.0` Release 仍只有 NSIS，因为 portable 步骤是在该 tag 后加入。
