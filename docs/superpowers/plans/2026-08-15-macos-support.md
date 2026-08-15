# macOS Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver a production-ready macOS 12+ EyesCare app with display filtering, scene rules, idle/event integration, menu-bar behavior, and signed/notarized Universal releases.

**Architecture:** Keep all product policy in `eyescare-core` and implement the existing platform traits in `eyescare-platform-mac`. Isolate Core Graphics/AppKit/Accessibility calls behind small adapters so conversion and state behavior are unit-testable, then select the macOS backends in the Tauri shell with target-specific compilation.

**Tech Stack:** Rust stable, Tauri 2.11, React 18, TypeScript 5.6, Core Graphics, AppKit, Accessibility, GitHub Actions.

## Global Constraints

- Minimum supported OS is macOS 12.0.
- Release binaries support both `aarch64-apple-darwin` and `x86_64-apple-darwin` in one Universal app.
- Use public Apple APIs only; no private frameworks, injection, kernel extensions, or App Store sandbox work.
- Basic foreground bundle ID detection must work without Accessibility or Screen Recording permission.
- Accessibility is optional and only improves fullscreen detection.
- Restore each display to the transfer table captured by EyesCare; do not call a global ColorSync reset in normal paths.
- A formal GitHub Release must fail closed when signing or notarization credentials are missing.
- Do not regress the Windows build, NSIS artifact, portable EXE ZIP, or existing config/rules schema.

---

### Task 1: Platform Contracts and macOS Module Boundaries

**Files:**
- Modify: `crates/eyescare-platform/src/foreground.rs`
- Modify: `crates/eyescare-platform/src/lib.rs`
- Modify: `crates/eyescare-platform-mac/Cargo.toml`
- Modify: `crates/eyescare-platform-mac/src/lib.rs`
- Create: `crates/eyescare-platform-mac/src/display.rs`
- Create: `crates/eyescare-platform-mac/src/display_math.rs`
- Create: `crates/eyescare-platform-mac/src/foreground.rs`
- Create: `crates/eyescare-platform-mac/src/system.rs`
- Create: `crates/eyescare-platform-mac/src/ffi.rs`
- Create: `crates/eyescare-platform-mac/src/unsupported.rs`

**Interfaces:**
- Produces: `ForegroundPermission::{NotRequired, Granted, Denied, Unknown}`.
- Produces: default trait methods `fullscreen_permission()` and `request_fullscreen_permission()` on `ForegroundAppBackend`.
- Produces: `MacDisplayBackend::new() -> eyescare_platform::Result<Self>`, `MacForegroundBackend::new() -> Self`, and `MacSystemBackend::new() -> Self`.
- Consumes: existing `DisplayBackend`, `ForegroundAppBackend`, and `SystemBackend` contracts.

- [ ] **Step 1: Add contract tests for optional foreground permission**

Add a test backend that only implements `foreground()` and assert the default methods return `NotRequired` without prompting. Add serde round-trip coverage for the four permission enum values.

Run: `cargo test -p eyescare-platform foreground`

Expected: FAIL because the permission enum and methods do not exist.

- [ ] **Step 2: Add the permission contract with non-breaking defaults**

Use this exact public shape:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForegroundPermission {
    NotRequired,
    Granted,
    Denied,
    Unknown,
}

pub trait ForegroundAppBackend: Send + Sync {
    fn foreground(&self) -> Result<SceneContext>;
    fn fullscreen_permission(&self) -> ForegroundPermission {
        ForegroundPermission::NotRequired
    }
    fn request_fullscreen_permission(&self) -> Result<ForegroundPermission> {
        Ok(self.fullscreen_permission())
    }
}
```

Re-export `ForegroundPermission` from `eyescare-platform/src/lib.rs`.

- [ ] **Step 3: Split the macOS crate while preserving non-macOS workspace builds**

On macOS, `lib.rs` exports `display`, `foreground`, and `system`. On other targets it exports the existing `Unsupported` implementations from `unsupported.rs`. Do not use `compile_error!`, because Windows workspace checks include every workspace member.

- [ ] **Step 4: Pin the already locked Apple binding family**

Add target-specific dependencies using the lock-compatible versions:

```toml
[target.'cfg(target_os = "macos")'.dependencies]
core-foundation = "0.10.1"
core-graphics = "0.25.0"
objc2 = "0.6.4"
objc2-app-kit = "0.3.2"
objc2-foundation = "0.3.2"
tracing = { workspace = true }
```

Enable only the generated framework features referenced by the implementation after checking each symbol against the selected crate version.

- [ ] **Step 5: Verify both host and macOS target resolution**

Run on Windows/Linux: `cargo test -p eyescare-platform -p eyescare-platform-mac`

Run on macOS: `cargo check -p eyescare-platform-mac --target aarch64-apple-darwin`

Expected: all commands exit 0; the non-macOS command exercises `unsupported.rs`.

- [ ] **Step 6: Commit**

```bash
git add crates/eyescare-platform crates/eyescare-platform-mac
git commit -m "refactor(mac): establish platform backend boundaries"
```

### Task 2: Transfer Table Conversion and Display API Adapter

**Files:**
- Modify: `crates/eyescare-platform-mac/src/display_math.rs`
- Modify: `crates/eyescare-platform-mac/src/ffi.rs`
- Modify: `crates/eyescare-platform-mac/src/display.rs`

**Interfaces:**
- Produces: `fn ramp_from_samples(red: &[f32], green: &[f32], blue: &[f32]) -> Result<Ramp>`.
- Produces: `fn ramp_to_samples(ramp: &Ramp) -> ([f32; 256], [f32; 256], [f32; 256])`.
- Produces: internal `DisplayApi` methods for enumerate, UUID, metadata, read table, write table, and EDR state.
- Consumes: `Ramp`, `DisplayId`, `DisplayInfo`, and `ApplyOutcome`.

- [ ] **Step 1: Write failing table conversion tests**

Cover 2-sample and 256-sample linear tables, clamping outside `[0, 1]`, rejection of empty/mismatched/non-finite channels, and `Ramp -> samples -> Ramp` maximum error of one `u16` unit.

Run: `cargo test -p eyescare-platform-mac display_math`

Expected: FAIL because the conversion functions do not exist.

- [ ] **Step 2: Implement deterministic conversion**

Use linear interpolation at `position = output_index * (input_len - 1) / 255`. Convert `f32` to `u16` with `value.clamp(0.0, 1.0) * 65535.0`, rounded to nearest. Return `GammaRejected` before conversion if any input is non-finite.

- [ ] **Step 3: Define a mockable display adapter**

Use this internal contract:

```rust
trait DisplayApi: Send + Sync {
    fn active_displays(&self) -> Result<Vec<u32>>;
    fn stable_uuid(&self, cg_id: u32) -> Result<Option<String>>;
    fn metadata(&self, cg_id: u32) -> Result<DisplayMetadata>;
    fn read_transfer(&self, cg_id: u32) -> Result<Ramp>;
    fn write_transfer(&self, cg_id: u32, ramp: &Ramp) -> Result<()>;
    fn edr_active(&self, cg_id: u32) -> Result<bool>;
}

struct DisplayMetadata {
    name: String,
    is_primary: bool,
    width: u32,
    height: u32,
    refresh_hz: Option<u32>,
}
```

`CoreGraphicsDisplayApi` is the only code allowed to call raw Apple APIs. `display.rs` owns policy and state.

- [ ] **Step 4: Implement Apple API calls with explicit OSStatus mapping**

Map non-success Core Graphics return values to `Error::Platform(format!("<operation> failed with CGError {code}"))`. Convert UUIDs to lowercase strings. Use `NSScreenNumber` to map `CGDirectDisplayID` to current EDR headroom; do not use maximum potential headroom as `hdr_active`.

- [ ] **Step 5: Run conversion tests and macOS check**

Run: `cargo test -p eyescare-platform-mac display_math`

Run on macOS: `cargo check -p eyescare-platform-mac --target aarch64-apple-darwin`

Expected: both exit 0.

- [ ] **Step 6: Commit**

```bash
git add crates/eyescare-platform-mac
git commit -m "feat(mac): add Core Graphics transfer adapter"
```

### Task 3: MacDisplayBackend State, Apply, Restore, and Rebind

**Files:**
- Modify: `crates/eyescare-platform-mac/src/display.rs`
- Test: `crates/eyescare-platform-mac/src/display.rs`

**Interfaces:**
- Consumes: `DisplayApi` from Task 2.
- Produces: complete `DisplayBackend` implementation and `MacDisplayBackend::new()`.

- [ ] **Step 1: Write failing mock-backend tests**

Add tests named:

```text
enumeration_uses_uuid_and_session_fallback
first_bind_captures_original_ramp
apply_reports_rejected_when_readback_does_not_change
apply_skips_only_current_edr_display
restore_all_attempts_every_display_after_one_failure
rebind_preserves_snapshot_by_stable_id
rebind_captures_snapshot_for_new_display
```

The mock records read/write calls and can inject errors per display.

Run on macOS: `cargo test -p eyescare-platform-mac display::tests`

Expected: FAIL until backend behavior is implemented.

- [ ] **Step 2: Implement construction and enumeration**

`MacDisplayBackend::new()` constructs `CoreGraphicsDisplayApi`, initializes `hdr_skip=true`, then calls `rebind_outputs()`. Stable IDs use `mac:<uuid>` or `mac:volatile:<cg_id>`. `list_displays()` reads current metadata and EDR state without mutating snapshots.

- [ ] **Step 3: Implement apply/readback classification**

Use `const READBACK_EPS: f64 = 0.02`, matching Windows. Return `HdrSkipped` before writing when required. Return `Rejected` on write error converted to a report only when the display still exists; return `DisplayGone` when the active display list no longer contains the ID.

- [ ] **Step 4: Implement best-effort restore**

`restore_all()` iterates all states, records the first error, continues restoring the rest, and returns the recorded error after the loop. It writes the captured snapshot or identity when no snapshot exists.

- [ ] **Step 5: Implement stable rebind**

Enumerate fresh state before taking the state mutex. Under the mutex, migrate snapshots by `DisplayId`; after releasing it, capture snapshots for new IDs. Reacquire only to store successful snapshots, preventing Apple calls while a shared state lock is held.

- [ ] **Step 6: Verify tests and clippy**

Run on macOS: `cargo test -p eyescare-platform-mac display::tests`

Run on macOS: `cargo clippy -p eyescare-platform-mac --all-targets -- -D warnings`

Expected: all tests pass and clippy exits 0.

- [ ] **Step 7: Commit**

```bash
git add crates/eyescare-platform-mac/src/display.rs
git commit -m "feat(mac): implement display filtering and recovery"
```

### Task 4: Foreground Bundle Identity and Optional Fullscreen Permission

**Files:**
- Modify: `crates/eyescare-platform-mac/src/foreground.rs`
- Modify: `crates/eyescare-platform-mac/src/ffi.rs`
- Test: `crates/eyescare-platform-mac/src/foreground.rs`

**Interfaces:**
- Consumes: `ForegroundPermission` from Task 1.
- Produces: `MacForegroundBackend` implementing identity and permission methods.

- [ ] **Step 1: Write failing scene mapping tests**

Use an internal `ForegroundApi` mock and cover: no active app, bundle ID/display name mapping, executable basename mapping, denied AX returning `Unknown`, AX fullscreen returning `High`, and geometry-only fullscreen returning `Medium`.

Run on macOS: `cargo test -p eyescare-platform-mac foreground::tests`

Expected: FAIL because the adapter and mappings do not exist.

- [ ] **Step 2: Implement zero-permission identity**

Inside `objc2::rc::autoreleasepool`, read `NSWorkspace.sharedWorkspace.frontmostApplication`. Populate `bundle_id`, `app_display_name`, and optional executable basename. Keep `window_title=None` and avoid CGWindowList APIs so Screen Recording is never requested.

- [ ] **Step 3: Implement explicit Accessibility behavior**

`fullscreen_permission()` calls `AXIsProcessTrusted()` without prompting. `request_fullscreen_permission()` calls `AXIsProcessTrustedWithOptions` with the prompt key set to true and returns the immediate state. Sampling only performs AX calls when already trusted.

- [ ] **Step 4: Implement fullscreen classification**

Read the focused window's full-screen attribute first. A true value maps to `(true, High, Borderless)`. Otherwise compare position/size to the containing NSScreen with a 2-point tolerance and map coverage to `(true, Medium, Borderless)`. All AX errors degrade to `(false, Unknown, None)`.

- [ ] **Step 5: Verify**

Run on macOS: `cargo test -p eyescare-platform-mac foreground::tests`

Run on macOS: `cargo clippy -p eyescare-platform-mac --all-targets -- -D warnings`

Expected: exit 0 without displaying a permission prompt during tests.

- [ ] **Step 6: Commit**

```bash
git add crates/eyescare-platform-mac/src/foreground.rs crates/eyescare-platform-mac/src/ffi.rs
git commit -m "feat(mac): add foreground app and fullscreen detection"
```

### Task 5: Idle Time and Display/Power Events

**Files:**
- Modify: `crates/eyescare-platform-mac/src/system.rs`
- Modify: `crates/eyescare-platform-mac/src/ffi.rs`
- Test: `crates/eyescare-platform-mac/src/system.rs`

**Interfaces:**
- Produces: `MacSystemBackend::new()` and complete `SystemBackend` behavior.
- Emits: `DisplayChanged`, `PowerResumed`, `SessionUnlocked`, and `SystemSleep`.

- [ ] **Step 1: Write failing event and idle tests**

Test finite idle rounding, negative/NaN/infinite idle fallback to 0, event mapping, and coalescing multiple display callbacks into one event inside 250ms.

Run on macOS: `cargo test -p eyescare-platform-mac system::tests`

Expected: FAIL until helpers exist.

- [ ] **Step 2: Implement idle time**

Call `CGEventSourceSecondsSinceLastEventType` for the combined session and any input event. Clamp finite values to `0..=u64::MAX` and truncate fractional seconds; return 0 for invalid values.

- [ ] **Step 3: Register event sources once**

Store callback state and observer tokens inside `MacSystemBackend`. Register `CGDisplayRegisterReconfigurationCallback` and workspace sleep/wake/session notifications on the main run loop. Reject a second subscription with `Error::Platform("macOS system events already subscribed")`.

- [ ] **Step 4: Implement shutdown cleanup**

On `Drop`, unregister the display callback and remove each workspace observer. Callback trampolines must never unwind across FFI; wrap user callback dispatch in `catch_unwind` and log failures.

- [ ] **Step 5: Keep self-start ownership explicit**

Return `Error::Unsupported` from `set_auto_start()` with a source comment that `tauri-plugin-autostart` is authoritative. Do not add a second LaunchAgent implementation.

- [ ] **Step 6: Verify**

Run on macOS: `cargo test -p eyescare-platform-mac system::tests`

Run on macOS: `cargo clippy -p eyescare-platform-mac --all-targets -- -D warnings`

Expected: exit 0.

- [ ] **Step 7: Commit**

```bash
git add crates/eyescare-platform-mac/src/system.rs crates/eyescare-platform-mac/src/ffi.rs
git commit -m "feat(mac): handle idle, display, and power events"
```

### Task 6: Select macOS Backends in the Tauri Shell

**Files:**
- Modify: `apps/desktop/src-tauri/Cargo.toml`
- Create: `apps/desktop/src-tauri/src/platform_backends.rs`
- Modify: `apps/desktop/src-tauri/src/lib.rs`
- Test: `apps/desktop/src-tauri/src/platform_backends.rs`

**Interfaces:**
- Consumes: all three macOS backends.
- Produces: `create_platform_backends() -> Result<PlatformBackends, String>`.
- Produces Tauri commands: `get_fullscreen_permission` and `request_fullscreen_permission`.

- [ ] **Step 1: Add a macOS target dependency**

```toml
[target.'cfg(target_os = "macos")'.dependencies]
eyescare-platform-mac = { path = "../../../crates/eyescare-platform-mac" }
```

- [ ] **Step 2: Extract backend selection**

Define:

```rust
pub struct PlatformBackends {
    pub display: Arc<dyn DisplayBackend>,
    pub foreground: Arc<dyn ForegroundAppBackend>,
    pub system: Arc<dyn SystemBackend>,
}
```

Provide Windows, macOS, and unsupported target implementations with `#[cfg]`. The macOS branch constructs concrete backends; setup logs and falls back to Noop only when construction returns an error.

- [ ] **Step 3: Add permission commands**

Commands call the two trait methods on `state.foreground` and serialize `ForegroundPermission`. Register both in `tauri::generate_handler!`. Do not request permission during startup.

- [ ] **Step 4: Add macOS menu-bar behavior**

During setup on macOS, set `ActivationPolicy::Accessory`. Before showing settings, activate the app and then show/focus the existing or new window. Configure the tray image as a template image only on macOS.

- [ ] **Step 5: Extend status payload**

Add top-level `platform: std::env::consts::OS` and include `bundle_id`, `fullscreen_confidence`, and `fullscreen_kind` in the existing scene JSON.

- [ ] **Step 6: Verify both desktop targets**

Run on Windows: `cargo check -p eyescare-desktop --target x86_64-pc-windows-msvc`

Run on macOS: `cargo check -p eyescare-desktop --target aarch64-apple-darwin`

Expected: both exit 0 and macOS no longer selects Noop under normal initialization.

- [ ] **Step 7: Commit**

```bash
git add apps/desktop/src-tauri
git commit -m "feat(mac): connect native backends to Tauri"
```

### Task 7: Bundle ID Rules and Accessibility Controls in Settings

**Files:**
- Modify: `apps/desktop/src/api.ts`
- Modify: `apps/desktop/src/App.tsx`
- Test: `apps/desktop/src/App.test.tsx` if a test runner is added; otherwise verify through TypeScript build and manual browser/Tauri checks.

**Interfaces:**
- Consumes: status platform/scene fields and permission commands from Task 6.
- Produces: rule field selector and optional fullscreen permission control.

- [ ] **Step 1: Extend TypeScript contracts**

Add `platform`, optional scene identity/fullscreen fields, `ForegroundPermission`, `getFullscreenPermission()`, and `requestFullscreenPermission()` to `api.ts`. The browser mock returns `not_required`.

- [ ] **Step 2: Generalize the new-rule form**

Replace `newProcess` with:

```ts
const [newField, setNewField] = useState<"process_name" | "bundle_id">("process_name");
const [newValue, setNewValue] = useState("");
```

After loading status, select `bundle_id` when `platform === "macos"`. Save the selected field in `if_.any[0]`; use `Code.exe` and `com.microsoft.VSCode` as platform-specific placeholders.

- [ ] **Step 3: Add the permission control**

Show the control only on macOS. Display Granted/Denied/Unknown without feature narration. The action button invokes `requestFullscreenPermission()`, refreshes status, and never loops or prompts on mount.

- [ ] **Step 4: Show current macOS identity**

In the current scene area, show bundle ID when present and retain process name as fallback. Keep window title absent.

- [ ] **Step 5: Verify frontend and Tauri integration**

Run: `cd apps/desktop && npm run build`

Run on macOS: `cd apps/desktop && npm run tauri -- dev`

Expected: TypeScript/Vite exit 0; a Bundle ID rule can be added/deleted and permission is only prompted after explicit action.

- [ ] **Step 6: Commit**

```bash
git add apps/desktop/src/api.ts apps/desktop/src/App.tsx
git commit -m "feat(mac): support bundle rules and fullscreen permission"
```

### Task 8: macOS Assets and Bundle Configuration

**Files:**
- Modify: `apps/desktop/src-tauri/tauri.conf.json`
- Create: `apps/desktop/src-tauri/icons/icon.icns`
- Create: `apps/desktop/src-tauri/icons/trayTemplate.png`
- Create: `apps/desktop/src-tauri/icons/trayTemplate@2x.png`
- Create: `apps/desktop/src-tauri/Entitlements.plist`

**Interfaces:**
- Produces: macOS application bundle resources and hardened runtime entitlements.

- [ ] **Step 1: Generate and inspect icon assets**

Generate `icon.icns` from the existing high-resolution source through Tauri's icon command. Create monochrome 18x18 and 36x36 alpha template tray images. Inspect them in both light and dark menu bars; reject colored or opaque-background variants.

- [ ] **Step 2: Make bundle targets command-driven**

Remove the global `"targets": ["nsis"]`; build jobs will pass platform-specific targets. Add `icons/icon.icns` to the icon list and set the macOS minimum system version to `12.0`.

- [ ] **Step 3: Add minimal hardened-runtime entitlements**

Use an empty entitlement dictionary unless a verified dependency requires a capability:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict/></plist>
```

Do not add app sandbox, disable-library-validation, Apple Events, camera, microphone, or screen recording entitlements.

- [ ] **Step 4: Build an unsigned local app**

Run on macOS:

```bash
export MACOSX_DEPLOYMENT_TARGET=12.0
cd apps/desktop
npm run tauri -- build --target aarch64-apple-darwin --bundles app
```

Expected: an `.app` exists under the target bundle directory and launches on macOS 12+.

- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src-tauri/tauri.conf.json apps/desktop/src-tauri/icons apps/desktop/src-tauri/Entitlements.plist
git commit -m "build(mac): add application and menu-bar assets"
```

### Task 9: macOS CI and Unified Release Publishing

**Files:**
- Modify: `.github/workflows/ci.yml`
- Modify: `.github/workflows/release.yml`

**Interfaces:**
- Produces artifacts: Windows NSIS, Windows portable ZIP, Universal macOS `.app.tar.gz`, and notarized `.dmg`.
- Consumes GitHub Secrets listed in `docs/macos-design.md`.

- [ ] **Step 1: Strengthen macOS CI**

Replace the stub-only build with core/platform tests, mac platform tests, clippy, frontend build, and an unsigned ARM app bundle. Keep Windows and frontend jobs intact.

Commands:

```bash
cargo test -p eyescare-core -p eyescare-platform -p eyescare-platform-mac
cargo clippy -p eyescare-platform-mac --all-targets -- -D warnings
cd apps/desktop && npm ci && npm run build
cd apps/desktop && npm run tauri -- build --target aarch64-apple-darwin --bundles app
```

- [ ] **Step 2: Refactor release into build and publish jobs**

Create `build-windows`, `build-macos`, and `publish` jobs. Build jobs upload artifacts with `actions/upload-artifact@v4`; `publish` depends on both, downloads them into `dist-release`, and invokes `softprops/action-gh-release@v2` once.

- [ ] **Step 3: Build Universal macOS artifacts**

On `macos-latest`, add both Rust targets and run:

```bash
rustup target add aarch64-apple-darwin x86_64-apple-darwin
export MACOSX_DEPLOYMENT_TARGET=12.0
cd apps/desktop
npm run tauri -- build --target universal-apple-darwin --bundles app,dmg
```

Archive the `.app` with `ditto -c -k --sequesterRsrc --keepParent` so resource forks and metadata survive ZIP extraction.

- [ ] **Step 4: Configure fail-closed signing and notarization**

Map the six secrets from the design document to Tauri's Apple signing environment variables. Add an early shell check that exits nonzero when a tag build lacks any secret. Do not execute this check in pull-request CI.

- [ ] **Step 5: Validate artifacts before upload**

Run in the macOS build job:

```bash
codesign --verify --deep --strict --verbose=2 "$APP_PATH"
spctl --assess --type execute --verbose=2 "$APP_PATH"
xcrun stapler validate "$DMG_PATH"
lipo -archs "$APP_PATH/Contents/MacOS/eyescare-desktop"
```

Expected architectures: `x86_64 arm64`; all verification commands exit 0.

- [ ] **Step 6: Preserve Windows portable verification**

Before zipping, execute the built EXE with a short smoke-test mode added by the desktop binary, or at minimum verify PE architecture and Authenticode status. Keep both NSIS and portable ZIP in the final artifact list.

- [ ] **Step 7: Validate workflow syntax**

Run: `actionlint .github/workflows/ci.yml .github/workflows/release.yml`

Expected: exit 0 with no findings.

- [ ] **Step 8: Commit**

```bash
git add .github/workflows/ci.yml .github/workflows/release.yml
git commit -m "ci: build and notarize Universal macOS releases"
```

### Task 10: End-to-End Verification and Beta Gate

**Files:**
- Modify: `README.md`
- Create: `docs/macos-qa.md`

**Interfaces:**
- Consumes: all prior tasks.
- Produces: reproducible automated evidence and signed beta QA record.

- [ ] **Step 1: Run the full automated suite**

On macOS:

```bash
cargo test -p eyescare-core -p eyescare-platform -p eyescare-platform-mac
cargo clippy -p eyescare-core -p eyescare-platform -p eyescare-platform-mac --all-targets -- -D warnings
cargo check -p eyescare-desktop --target aarch64-apple-darwin
cd apps/desktop && npm run build
git diff --check
```

Expected: every command exits 0 and tests report zero failures.

- [ ] **Step 2: Run the Windows regression gate**

On Windows CI:

```powershell
cargo test -p eyescare-core -p eyescare-platform
cargo build -p eyescare-platform-win
cargo check -p eyescare-desktop --target x86_64-pc-windows-msvc
Set-Location apps/desktop
npm run build
```

Expected: every command exits 0.

- [ ] **Step 3: Execute the manual hardware matrix**

Record OS/build, hardware, internal/external display, EDR state, permission state, action, expected result, actual result, and logs in `docs/macos-qa.md`. Test every row in the design document on one Apple Silicon Mac and one Intel Mac; include at least one external display run.

- [ ] **Step 4: Validate the signed release from a clean account**

Download the GitHub artifact, install from DMG, launch through Finder, enable/disable Accessibility, enable autostart, reboot, and uninstall. Confirm no Gatekeeper override is required and the display restores after quit.

- [ ] **Step 5: Update local development documentation**

Add macOS 12+, Xcode Command Line Tools, Node 20, `npm run tauri -- dev`, platform test commands, and the optional Accessibility behavior to README. Keep README limited to features and local development as required by the existing project convention.

- [ ] **Step 6: Commit**

```bash
git add README.md docs/macos-qa.md
git commit -m "docs(mac): add development and beta verification guide"
```

## Self-Review Checklist

- [ ] Every macOS design goal maps to at least one task.
- [ ] No task requires Screen Recording, private APIs, or App Sandbox.
- [ ] Windows build, NSIS, and portable ZIP remain explicit gates.
- [ ] Permission denial has a tested degraded path.
- [ ] Display restore attempts every active display and never uses global reset.
- [ ] Release publication waits for both Windows and macOS artifacts.
- [ ] Commands, file paths, interfaces, and expected outcomes are concrete.
