//! EyesCare Tauri 壳（MVP-A 完全体接线）。
//!
//! 职责：
//! - 命令层：显示/旁路/规则/计时/昼夜/洞察/导入导出（全部持久化）
//! - 主循环（1s）：display pump、SafeMode tick、Timer tick（idle 暂停）、
//!   Scene 采样（750ms）→ RuleEngine 决策 → SafeMode/claims/breaks 联动、
//!   DayNight 计算、Insights heartbeat/rollup
//! - 崩溃/退出恢复：panic hook + RunEvent::Exit / 会话结束 restore_all
//!
//! 注意：本 crate 需 Windows 真机（或带 webkit2gtk 的环境）完整构建；
//! CI windows-latest 验证。core 逻辑全部在 eyescare-core，独立测试。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use eyescare_core::config::{
    AppConfig, ConfigStore, DayNightConfig, ExportBundle, ImportOutcome, RulesConfig, TimerConfig,
};
use eyescare_core::display::{Claim, DisplayService};
use eyescare_core::guided::GuidedBreakPlayer;
use eyescare_core::insights::{HeartbeatSample, InsightsStore, TodaySummary};
use eyescare_core::rules::{RuleEngine, builtin_templates, merge_templates};
use eyescare_core::safe_mode::{SafeModeController, SafeModeStatus};
use eyescare_core::scene::SceneSnapshot;
use eyescare_core::scene_engine::{self, BreaksPolicy};
use eyescare_core::timer::{TimerEvent, TimerService, TimerState};
use eyescare_platform::{DisplayBackend, ForegroundAppBackend, SystemBackend, SystemEvent};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

/// 锁中毒后仍取出内部值，避免 tick 线程永久停摆。
fn lock_mutex<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// 应用共享状态。
pub struct AppState {
    pub display: Arc<DisplayService>,
    pub safe_mode: Mutex<SafeModeController>,
    pub timer: Mutex<TimerService>,
    pub config: Mutex<AppConfig>,
    pub rules: Mutex<RulesConfig>,
    pub store: ConfigStore,
    pub insights: Mutex<Option<InsightsStore>>,
    pub engine: Mutex<RuleEngine>,
    pub scene: Mutex<Option<SceneSnapshot>>,
    pub last_scene_key: Mutex<Option<String>>,
    pub breaks_policy: Mutex<BreaksPolicy>,
    pub guided: Mutex<Option<GuidedBreakPlayer>>,
    pub system: Arc<dyn SystemBackend>,
    pub foreground: Arc<dyn ForegroundAppBackend>,
    /// 最近一次 DayNight 目标（避免每 tick recompute）。
    pub last_daynight_kelvin: Mutex<Option<u32>>,
    /// Insights heartbeat 累计秒。
    pub heartbeat_acc: Mutex<u64>,
    /// 上次 purge 的本地日（YYYY-MM-DD）；启动 + 跨日各执行一次。
    pub last_purge_day: Mutex<String>,
    /// 用户「滤镜开关 / 恢复显示」：false 时不再自动施加 ramp。
    pub filter_enabled: AtomicBool,
    /// 托盘「退出」置位，允许进程真正结束。
    pub quitting: AtomicBool,
    /// 上次已应用到 DisplayService 的规则声明签名（避免每秒 recompute）。
    pub last_claim_sig: Mutex<Option<(Option<(String, String)>, bool, Option<String>)>>,
}

// ---------- 显示命令 ----------

#[tauri::command]
fn get_status(state: State<'_, AppState>) -> serde_json::Value {
    status_json(&state)
}

fn status_json(state: &State<'_, AppState>) -> serde_json::Value {
    let sm = lock_mutex(&state.safe_mode).status();
    let timer = lock_mutex(&state.timer).status();
    let cfg = lock_mutex(&state.config);
    let scene = lock_mutex(&state.scene).clone();
    serde_json::json!({
        "safe_mode": sm,
        "timer": timer,
        "display": {
            "kelvin": cfg.display.kelvin,
            "brightness": cfg.display.brightness,
            "preset": cfg.display.preset,
            "day_night_enabled": cfg.display.day_night.enabled,
        },
        "scene": scene.map(|s| serde_json::json!({
            "process_name": s.process_name,
            "app_display_name": s.app_display_name,
            "is_fullscreen": s.is_fullscreen,
        })),
        "breaks_policy": format!("{:?}", *lock_mutex(&state.breaks_policy)),
        "filter_enabled": state.filter_enabled.load(Ordering::Relaxed),
        "guided": lock_mutex(&state.guided).as_ref().and_then(|p| {
            p.current_step().map(|s| serde_json::json!({
                "title": s.title,
                "body": s.body,
                "remaining_sec": p.step_remaining_sec(),
            }))
        }),
    })
}

/// 用户手动设置色温/亮度（P2 UserLock + 持久化）。
#[tauri::command]
fn set_display_params(
    state: State<'_, AppState>,
    kelvin: u32,
    brightness: f64,
) -> Result<(), String> {
    let k = kelvin.clamp(1000, 10000);
    let b = brightness.clamp(0.01, 1.0);
    let mut cfg = state.config.lock().unwrap();
    cfg.display.kelvin = k;
    cfg.display.brightness = b;
    cfg.display.preset = "custom".into();
    cfg.display.manual_lock = true;
    state.store.save_config(&cfg).map_err(|e| e.to_string())?;
    drop(cfg);
    // P2 claim（对所有已枚举屏）
    let ids = display_ids(&state);
    let claims: Vec<(String, Claim)> = ids
        .iter()
        .map(|id| (id.clone(), Claim::user_lock(k, b)))
        .collect();
    state.filter_enabled.store(true, Ordering::Relaxed);
    state.display.set_applies_enabled(true);
    state.display.set_source("user", claims);
    let _ = state.display.recompute();
    Ok(())
}

/// 取消用户锁定（回落到规则/昼夜/默认）。
#[tauri::command]
fn release_manual_lock(state: State<'_, AppState>) -> Result<(), String> {
    let mut cfg = state.config.lock().unwrap();
    cfg.display.manual_lock = false;
    state.store.save_config(&cfg).map_err(|e| e.to_string())?;
    drop(cfg);
    state.filter_enabled.store(true, Ordering::Relaxed);
    state.display.set_applies_enabled(true);
    state.display.remove_source("user");
    let _ = state.display.recompute();
    Ok(())
}

/// 预设切换（附录 A 表；"smart" 跟随 DayNight，不抢 P2 锁）。
#[tauri::command]
fn set_preset(state: State<'_, AppState>, preset: String) -> Result<(), String> {
    apply_preset(&state, preset)
}

fn apply_preset(state: &State<'_, AppState>, preset: String) -> Result<(), String> {
    let is_smart = preset == "smart";
    {
        let mut cfg = lock_mutex(&state.config);
        let (k, b) = preset_params(&preset, &cfg);
        cfg.display.preset = preset;
        cfg.display.kelvin = k;
        cfg.display.brightness = b;
        cfg.display.manual_lock = !is_smart;
        if is_smart {
            cfg.display.day_night.enabled = true;
        }
        state.store.save_config(&cfg).map_err(|e| e.to_string())?;
    }
    state.filter_enabled.store(true, Ordering::Relaxed);
    state.display.set_applies_enabled(true);
    if is_smart {
        state.display.remove_source("user");
        update_daynight_claims(&state);
    } else {
        let (k2, b2) = preset_params_current(&state);
        let ids = display_ids(&state);
        let claims: Vec<(String, Claim)> = ids
            .iter()
            .map(|id| (id.clone(), Claim::user_lock(k2, b2)))
            .collect();
        state.display.set_source("user", claims);
    }
    let _ = state.display.recompute();
    Ok(())
}

#[tauri::command]
fn restore_display(state: State<'_, AppState>) -> Result<(), String> {
    state.filter_enabled.store(false, Ordering::Relaxed);
    *lock_mutex(&state.last_daynight_kelvin) = None;
    state.display.restore_all().map_err(|e| e.to_string())
}

#[tauri::command]
fn set_filter_enabled(app: AppHandle, state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    set_filter(&app, &state, enabled);
    Ok(())
}

// ---------- 旁路命令 ----------

#[tauri::command]
fn safe_mode_enter(app: AppHandle, state: State<'_, AppState>) {
    state.safe_mode.lock().unwrap().user_enter();
    apply_safe_claims(&state);
    emit_status(&app, &state);
}

#[tauri::command]
fn safe_mode_exit(app: AppHandle, state: State<'_, AppState>) {
    state.safe_mode.lock().unwrap().user_exit();
    apply_safe_claims(&state);
    emit_status(&app, &state);
}

#[tauri::command]
fn safe_mode_extend(app: AppHandle, state: State<'_, AppState>, minutes: u64) {
    state.safe_mode.lock().unwrap().user_extend(minutes);
    emit_status(&app, &state);
}

#[tauri::command]
fn safe_mode_status(state: State<'_, AppState>) -> SafeModeStatus {
    state.safe_mode.lock().unwrap().status()
}

// ---------- 规则命令 ----------

#[tauri::command]
fn get_rules(state: State<'_, AppState>) -> RulesConfig {
    state.rules.lock().unwrap().clone()
}

#[tauri::command]
fn save_rules(state: State<'_, AppState>, rules: RulesConfig) -> Result<(), String> {
    rules.validate_all().map_err(|e| e.to_string())?;
    state
        .store
        .save_rules(&rules)
        .map_err(|e| e.to_string())?;
    *lock_mutex(&state.rules) = rules;
    refresh_engine(&state);
    Ok(())
}

/// 单条规则启停（完全体：落盘 + 引擎热更新）。
#[tauri::command]
fn set_rule_enabled(
    state: State<'_, AppState>,
    rule_id: String,
    enabled: bool,
) -> Result<(), String> {
    let mut rules = state.rules.lock().unwrap();
    let mut found = false;
    for r in rules.rules.iter_mut() {
        if r.id == rule_id {
            r.enabled = Some(enabled);
            found = true;
            break;
        }
    }
    if !found {
        return Err(format!("rule not found: {rule_id}"));
    }
    state
        .store
        .save_rules(&rules)
        .map_err(|e| e.to_string())?;
    drop(rules);
    refresh_engine(&state);
    Ok(())
}

#[tauri::command]
fn install_templates(state: State<'_, AppState>) -> Result<usize, String> {
    let mut rules = state.rules.lock().unwrap();
    let before = rules.rules.len();
    merge_templates(&mut rules.rules, builtin_templates()).map_err(|e| e.to_string())?;
    let added = rules.rules.len() - before;
    state.store.save_rules(&rules).map_err(|e| e.to_string())?;
    drop(rules);
    refresh_engine(&state);
    Ok(added)
}

// ---------- 计时 / 昼夜命令 ----------

#[tauri::command]
fn set_timer_config(app: AppHandle, state: State<'_, AppState>, config: TimerConfig) -> Result<(), String> {
    // 校验边界（与 config.validated 一致；直接构造不经过 validated）
    if config.work_sec < 30 || config.break_sec < 5 || config.idle_pause_sec < 10 {
        return Err("invalid timer config: work_sec>=30, break_sec>=5, idle_pause_sec>=10".into());
    }
    {
        let mut cfg = lock_mutex(&state.config);
        cfg.timer = config.clone();
        state.store.save_config(&cfg).map_err(|e| e.to_string())?;
    }
    lock_mutex(&state.timer).apply_config(config);
    emit_status(&app, &state);
    Ok(())
}

#[tauri::command]
fn set_day_night_config(state: State<'_, AppState>, config: DayNightConfig) -> Result<(), String> {
    day_night_valid(&config)?;
    {
        let mut cfg = state.config.lock().unwrap();
        cfg.display.day_night = config;
        state.store.save_config(&cfg).map_err(|e| e.to_string())?;
    }
    // 立即重算昼夜（滤镜关闭时不回写 ramp）
    if state.filter_enabled.load(Ordering::Relaxed) {
        update_daynight_claims(&state);
        let _ = state.display.recompute();
    }
    Ok(())
}

/// 与 `AppConfig::validated` 的 day_night 字段规则一致。
fn day_night_valid(cfg: &DayNightConfig) -> Result<(), String> {
    if cfg.transition_minutes == 0 {
        return Err("day_night.transition_minutes must be > 0".into());
    }
    if !day_night_hhmm(&cfg.day_start) {
        return Err(format!(
            "day_night.day_start must be HH:MM, got {:?}",
            cfg.day_start
        ));
    }
    if !day_night_hhmm(&cfg.night_start) {
        return Err(format!(
            "day_night.night_start must be HH:MM, got {:?}",
            cfg.night_start
        ));
    }
    Ok(())
}

fn day_night_hhmm(s: &str) -> bool {
    let mut it = s.split(':');
    let (Some(h), Some(m), None) = (it.next(), it.next(), it.next()) else {
        return false;
    };
    let (Ok(h), Ok(m)) = (h.parse::<u32>(), m.parse::<u32>()) else {
        return false;
    };
    h < 24 && m < 60
}

/// 多屏模式切换（Sync/PerDisplay）。
#[tauri::command]
fn set_multi_monitor(
    state: State<'_, AppState>,
    mode: String,
) -> Result<(), String> {
    let m = match mode.as_str() {
        "sync" => eyescare_core::config::MultiMonitorMode::Sync,
        "per_display" => eyescare_core::config::MultiMonitorMode::PerDisplay,
        _ => return Err(format!("unknown multi_monitor mode: {mode}")),
    };
    let mut cfg = state.config.lock().unwrap();
    cfg.display.multi_monitor = m;
    state.store.save_config(&cfg).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn set_hdr_policy(state: State<'_, AppState>, policy: String) -> Result<(), String> {
    let p = match policy.as_str() {
        "skip" => eyescare_core::config::HdrPolicy::Skip,
        "force" => eyescare_core::config::HdrPolicy::Force,
        _ => return Err(format!("unknown hdr_policy: {policy}")),
    };
    let mut cfg = lock_mutex(&state.config);
    cfg.display.hdr_policy = p;
    state.store.save_config(&cfg).map_err(|e| e.to_string())?;
    drop(cfg);
    state
        .display
        .set_hdr_skip(matches!(p, eyescare_core::config::HdrPolicy::Skip));
    Ok(())
}

/// 保存快捷键配置。全局快捷键由前端插件注册，格式由插件校验。
#[tauri::command]
fn set_shortcuts(
    app: AppHandle,
    state: State<'_, AppState>,
    toggle_filter: String,
    toggle_safe_mode: String,
    start_break: String,
) -> Result<(), String> {
    let values = [&toggle_filter, &toggle_safe_mode, &start_break];
    if values.iter().any(|v| v.trim().is_empty()) {
        return Err("shortcut must not be empty".into());
    }
    if toggle_filter == toggle_safe_mode
        || toggle_filter == start_break
        || toggle_safe_mode == start_break
    {
        return Err("shortcuts must be unique".into());
    }
    let next = eyescare_core::config::ShortcutConfig {
        toggle_filter,
        toggle_safe_mode,
        start_break,
    };
    let previous = lock_mutex(&state.config).shortcuts.clone();
    register_shortcuts(&app, &next)?;
    let mut cfg = lock_mutex(&state.config);
    cfg.shortcuts = next;
    if let Err(error) = state.store.save_config(&cfg) {
        cfg.shortcuts = previous.clone();
        drop(cfg);
        let _ = register_shortcuts(&app, &previous);
        return Err(error.to_string());
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum HotkeyAction {
    ToggleFilter,
    ToggleSafeMode,
    StartBreak,
}

/// 全局快捷键在 Rust 进程中注册，设置窗口关闭后仍持续有效。
fn register_shortcuts(
    app: &AppHandle,
    shortcuts: &eyescare_core::config::ShortcutConfig,
) -> Result<(), String> {
    let manager = app.global_shortcut();
    manager.unregister_all().map_err(|e| e.to_string())?;
    // 不要用 shortcut.to_string() 和用户配置比：内部是 control+alt+KeyF，配置是 Ctrl+Alt+F。
    let bindings = [
        (shortcuts.toggle_filter.as_str(), HotkeyAction::ToggleFilter),
        (shortcuts.toggle_safe_mode.as_str(), HotkeyAction::ToggleSafeMode),
        (shortcuts.start_break.as_str(), HotkeyAction::StartBreak),
    ];
    for (combo, action) in bindings {
        manager
            .on_shortcut(combo, move |app, _shortcut, event| {
                if event.state != ShortcutState::Pressed {
                    return;
                }
                let state = app.state::<AppState>();
                match action {
                    HotkeyAction::ToggleFilter => {
                        let enabled = !state.filter_enabled.load(Ordering::Relaxed);
                        set_filter(app, &state, enabled);
                    }
                    HotkeyAction::ToggleSafeMode => {
                        let mut safe = lock_mutex(&state.safe_mode);
                        if safe.is_active() {
                            safe.user_exit();
                        } else {
                            safe.user_enter();
                        }
                        drop(safe);
                        apply_safe_claims(&state);
                        emit_status(app, &state);
                    }
                    HotkeyAction::StartBreak => {
                        start_user_guided_break(app, &state);
                    }
                }
            })
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

// ---------- 洞察 / 配置命令 ----------

#[tauri::command]
fn get_today_summary(state: State<'_, AppState>) -> Result<TodaySummary, String> {
    let insights = state.insights.lock().unwrap();
    let Some(db) = insights.as_ref() else {
        return Ok(TodaySummary::default());
    };
    db.today_summary().map_err(|e| e.to_string())
}

#[tauri::command]
fn get_full_config(state: State<'_, AppState>) -> AppConfig {
    state.config.lock().unwrap().clone()
}

#[tauri::command]
fn export_config(state: State<'_, AppState>) -> Result<ExportBundle, String> {
    state.store.export().map_err(|e| e.to_string())
}

#[tauri::command]
fn import_config(app: AppHandle, state: State<'_, AppState>, bundle: ExportBundle) -> Result<ImportOutcome, String> {
    let outcome = state.store.import(&bundle).map_err(|e| e.to_string())?;
    reload_config_into_state(&app, &state)?;
    Ok(outcome)
}

#[tauri::command]
fn skip_break(app: AppHandle, state: State<'_, AppState>) {
    end_break_from_ui(&app, &state);
}

#[tauri::command]
fn start_guided_break(app: AppHandle, state: State<'_, AppState>) {
    start_user_guided_break(&app, &state);
}

#[derive(Clone, Copy)]
enum OverlayPolicy {
    /// 自动到点：尊重「引导 / 静默」开关。
    Auto,
    /// 按钮 / 快捷键 / 托盘：强制弹出引导窗。
    Force,
}

fn start_user_guided_break(app: &AppHandle, state: &State<'_, AppState>) {
    let events = {
        let mut timer = lock_mutex(&state.timer);
        timer.start_break_now();
        timer.drain_events()
    };
    for event in events {
        handle_timer_event(app, state, event, OverlayPolicy::Force);
    }
    emit_status(app, state);
}

fn end_break_from_ui(app: &AppHandle, state: &State<'_, AppState>) {
    let events = {
        let mut timer = lock_mutex(&state.timer);
        timer.skip();
        timer.drain_events()
    };
    for event in events {
        handle_timer_event(app, state, event, OverlayPolicy::Auto);
    }
    *lock_mutex(&state.guided) = None;
    close_break_overlay(app);
    emit_status(app, state);
}

// ---------- 主循环 ----------

fn spawn_tick(app: AppHandle) {
    std::thread::Builder::new()
        .name("eyescare-tick".into())
        .spawn(move || {
            let mut consecutive_failures: u32 = 0;
            loop {
                std::thread::sleep(Duration::from_secs(1));
                // 主循环必须存活：panic 时记录并重启（退避 1s/5s/30s），
                // 避免锁中毒或单次错误导致显示/计时/洞察全部停摆。
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    tick_once(&app);
                }));
                if result.is_err() {
                    consecutive_failures += 1;
                    let backoff = match consecutive_failures {
                        1 => 1,
                        2 => 5,
                        _ => 30,
                    };
                    tracing::error!(
                        "tick panic (failure #{consecutive_failures}); restarting in {backoff}s"
                    );
                    std::thread::sleep(Duration::from_secs(backoff));
                } else {
                    consecutive_failures = 0;
                }
            }
        })
        .ok();
}

/// 单次主循环（1s 粒度；场景采样内部按 750ms 节拍折算为每 3 tick）。
fn tick_once(app: &AppHandle) {
    let state = app.state::<AppState>();
    let filter_on = state.filter_enabled.load(Ordering::Relaxed);

    // 1) 显示动画 pump（无动画时跳过，少一次 ramp map 克隆）
    if filter_on && state.display.is_animating() {
        let _ = state.display.pump();
    }

    // 2) SafeMode duration tick — 必须先释放锁再 apply/emit，否则非可重入 Mutex 死锁
    let sm_expired = {
        let mut sm = lock_mutex(&state.safe_mode);
        let changed = sm.tick();
        if changed {
            let key = lock_mutex(&state.scene).as_ref().and_then(|s| s.app_key());
            sm.set_cooldown_key(key);
        }
        changed
    };
    if sm_expired {
        if filter_on {
            apply_safe_claims(&state);
        }
        emit_status(app, &state);
    }

    // 3) 前台采样 + 场景决策（主循环 1s 粒度；设计 750ms 节拍，壳层 1s tick 即最密节拍）
    scene_step(app, &state);

    // 4) DayNight（30s 节拍；滤镜关闭时不得回写）
    {
        let mut acc = state.heartbeat_acc.lock().unwrap();
        if filter_on && *acc % 30 == 0 {
            update_daynight_claims(&state);
        }
        *acc += 1;
    }

    // 5) Timer tick（idle + breaks policy）
    let timer_events: Vec<TimerEvent> = {
        let mut timer = state.timer.lock().unwrap();
        let idle_sec = state.system.seconds_since_input();
        if idle_sec < 2 {
            timer.note_activity();
        }
        let policy = *state.breaks_policy.lock().unwrap();
        timer.tick(Instant::now(), match policy {
            BreaksPolicy::Keep => eyescare_core::timer::BreaksPolicy::Keep,
            BreaksPolicy::Pause => eyescare_core::timer::BreaksPolicy::Pause,
            BreaksPolicy::NotifyOnly => eyescare_core::timer::BreaksPolicy::NotifyOnly,
        })
    };
    for ev in timer_events {
        handle_timer_event(app, &state, ev, OverlayPolicy::Auto);
    }

    let timer_state = lock_mutex(&state.timer).status().state;
    if matches!(timer_state, TimerState::Break | TimerState::PreBreak) {
        emit_status(app, &state);
    }

    // 6) Insights heartbeat（60s 一次）+ 引导休息推进
    {
        let mut guided = state.guided.lock().unwrap();
        if let Some(player) = guided.as_mut() {
            if player.tick() {
                emit_status(app, &state);
            }
            if player.state() == eyescare_core::guided::GuidedState::Done {
                *guided = None;
            }
        }
    }
    heartbeat_step(&state);
}

/// 场景采样 → 决策 → 执行（SceneEngine 联动）。
fn scene_step(app: &AppHandle, state: &State<'_, AppState>) {
    let scene = state
        .foreground
        .foreground()
        .ok()
        .map(|c| SceneSnapshot::from_platform(&c));
    *state.scene.lock().unwrap() = scene.clone();

    let decision = {
        let engine = state.engine.lock().unwrap();
        let last_key = state.last_scene_key.lock().unwrap().clone();
        scene_engine::decide(&engine, scene.as_ref(), &last_key)
    };

    // 更新 last key（scene_changed 依据）
    if decision.scene_changed {
        *state.last_scene_key.lock().unwrap() = decision.app_key.clone();
    }

    // SafeMode 规则联动（先算完再释放锁，避免 apply_safe_claims / emit_status 重入死锁）
    let sm_changed = {
        let mut sm = lock_mutex(&state.safe_mode);
        let key = decision.app_key.as_deref();
        if let Some(rule_id) = &decision.rule_safe_mode {
            let (hold, minutes) = decision
                .matched
                .as_ref()
                .and_then(|m| match &m.rule.then {
                    eyescare_core::rules::RuleAction::SafeMode { hold, minutes } => {
                        Some((*hold, minutes.unwrap_or(0) as u64))
                    }
                    _ => None,
                })
                .unwrap_or((eyescare_core::config::SafeModeHold::WhileMatch, 0));
            sm.rule_eval(Some(rule_id), hold, key, minutes)
        } else {
            sm.scene_changed(false, decision.scene_changed)
        }
    };
    if sm_changed {
        if state.filter_enabled.load(Ordering::Relaxed) {
            apply_safe_claims(state);
        }
        emit_status(app, state);
    }

    // 规则预设 / 全屏策略 claims：声明未变则跳过 set_source + recompute
    let sig = (
        decision.rule_preset.clone(),
        decision.filter_paused,
        decision.rule_safe_mode.clone(),
    );
    let claims_changed = {
        let mut last = lock_mutex(&state.last_claim_sig);
        if last.as_ref() == Some(&sig) {
            false
        } else {
            *last = Some(sig);
            true
        }
    };

    if state.filter_enabled.load(Ordering::Relaxed) && claims_changed {
        let ids = display_ids(state);
        if let Some((rule_id, preset)) = &decision.rule_preset {
            let (k, b) = preset_params(preset, &lock_mutex(&state.config));
            let claims: Vec<(String, Claim)> = ids
                .iter()
                .map(|id| (id.clone(), Claim::rule_preset(rule_id, k, b)))
                .collect();
            state.display.set_source("rule", claims);
        } else {
            state.display.remove_source("rule");
        }
        if decision.filter_paused {
            let claims: Vec<(String, Claim)> = ids
                .iter()
                .map(|id| (id.clone(), Claim::safe_bypass()))
                .collect();
            state.display.set_source("policy_pause", claims);
        } else {
            state.display.remove_source("policy_pause");
        }
        if state.filter_enabled.load(Ordering::Relaxed) {
            let _ = state.display.recompute();
        }
    }

    *lock_mutex(&state.breaks_policy) = decision.breaks_policy;
}

/// SafeMode P1 claims 同步到 DisplayService。
fn apply_safe_claims(state: &State<'_, AppState>) {
    if !state.filter_enabled.load(Ordering::Relaxed) {
        return;
    }
    let sm = lock_mutex(&state.safe_mode);
    let ids = display_ids(state);
    let claims = sm.p1_claims(&ids);
    drop(sm);
    if claims.is_empty() {
        state.display.remove_source("safe");
    } else {
        state.display.set_source("safe", claims);
    }
    if !state.filter_enabled.load(Ordering::Relaxed) {
        return;
    }
    let _ = state.display.recompute();
}

/// DayNight P4 claim 更新（目标变化才 recompute）。
fn update_daynight_claims(state: &State<'_, AppState>) {
    if !state.filter_enabled.load(Ordering::Relaxed) {
        return;
    }
    let cfg = state.config.lock().unwrap();
    let dn = &cfg.display.day_night;
    if !dn.enabled {
        *state.last_daynight_kelvin.lock().unwrap() = None;
        state.display.remove_source("daynight");
        return;
    }
    let k = eyescare_core::display::day_night::day_night_kelvin(
        dn,
        eyescare_core::display::day_night::now_local(),
    );
    let changed = {
        let last = state.last_daynight_kelvin.lock().unwrap();
        *last != Some(k)
    };
    if !changed {
        return;
    }
    let ids = display_ids(state);
    let claims: Vec<(String, Claim)> = ids
        .iter()
        .map(|id| (id.clone(), Claim::day_night(k)))
        .collect();
    if !state.filter_enabled.load(Ordering::Relaxed) {
        *state.last_daynight_kelvin.lock().unwrap() = None;
        return;
    }
    state.display.set_source("daynight", claims);
    if let Ok(summary) = state.display.recompute() {
        if !summary.any_failure() {
            *state.last_daynight_kelvin.lock().unwrap() = Some(k);
        }
    }
}

/// Insights heartbeat（60s；idle>5min 记不活跃）+ 每日 rollup + 每日 purge。
fn heartbeat_step(state: &State<'_, AppState>) {
    let insights = state.insights.lock().unwrap();
    let Some(db) = insights.as_ref() else {
        return;
    };
    let now = chrono::Utc::now().timestamp();
    let acc = state.heartbeat_acc.lock().unwrap();
    let every_60 = acc.is_multiple_of(60);
    if every_60 {
        let idle_sec = state.system.seconds_since_input();
        let active = idle_sec < 5 * 60; // 5 分钟无输入视为不活跃（锁屏/离开）
        let sm = state.safe_mode.lock().unwrap();
        let cfg = state.config.lock().unwrap();
        let filter_on = active
            && !sm.is_active()
            && !state.display.known_displays().is_empty()
            && !state
                .display
                .current_targets()
                .values()
                .all(|r| r.is_identity());
        let _ = db.heartbeat(HeartbeatSample {
            ts_utc: now,
            active,
            filter_on,
            kelvin: Some(cfg.display.kelvin),
            brightness: Some(cfg.display.brightness),
        });
        // 跨日时 rollup 昨日 + 今日
        let day = chrono::Local::now().date_naive();
        let _ = db.rollup(day);
        let _ = db.rollup(day - chrono::Days::new(1));
        // purge 每日一次（跨日才执行；启动时已 purge 一次）
        let day_str = day.format("%Y-%m-%d").to_string();
        let mut last_purge = state.last_purge_day.lock().unwrap();
        if *last_purge != day_str {
            let _ = db.purge(cfg.insights.retention_days.max(30));
            *last_purge = day_str;
        }
        drop(acc);
    }
}

fn notify_user(app: &AppHandle, body: &str) {
    use tauri_plugin_notification::NotificationExt;
    if let Err(e) = app
        .notification()
        .builder()
        .title("EyesCare")
        .body(body)
        .show()
    {
        tracing::warn!("notification failed: {e}");
    }
}

fn handle_timer_event(
    app: &AppHandle,
    state: &State<'_, AppState>,
    ev: TimerEvent,
    overlay: OverlayPolicy,
) {
    match ev {
        TimerEvent::BreakDue => {
            let _ = app.emit("timer-event", "break_due");
            // Keep 策略下紧接着会有 BreakStarted，只留一条休息通知。
            let already_in_break = lock_mutex(&state.timer).state() == TimerState::Break;
            if !already_in_break {
                notify_user(app, "休息时间到了：远眺 6 米，起身活动一下");
            }
            if let Some(db) = lock_mutex(&state.insights).as_ref() {
                let now = chrono::Utc::now().timestamp();
                let _ = db.record_break(now, "break_prompted");
            }
        }
        TimerEvent::BreakStarted => {
            let cfg = lock_mutex(&state.config);
            let silent = matches!(overlay, OverlayPolicy::Auto)
                && (cfg.timer.silent_break || !cfg.timer.guided);
            let profile = cfg.timer.profile;
            let break_sec = cfg.timer.break_sec;
            drop(cfg);
            notify_user(app, "休息开始：远眺 6 米，起身活动一下");
            if !silent {
                let steps = match profile {
                    eyescare_core::config::TimerProfile::TwentyTwentyTwenty => {
                        eyescare_core::guided::twenty_twenty_timeline(break_sec as u64)
                    }
                    eyescare_core::config::TimerProfile::Normal => {
                        eyescare_core::guided::normal_timeline(break_sec as u64)
                    }
                };
                let mut player = GuidedBreakPlayer::new(steps);
                player.start();
                *lock_mutex(&state.guided) = Some(player);
                show_break_overlay(app);
            }
            let _ = app.emit("timer-event", "break_started");
        }
        TimerEvent::BreakFinished { completed } => {
            *lock_mutex(&state.guided) = None;
            close_break_overlay(app);
            let _ = app.emit("timer-event", serde_json::json!({"break_finished": completed}).to_string());
            if let Some(db) = lock_mutex(&state.insights).as_ref() {
                let now = chrono::Utc::now().timestamp();
                let _ = db.record_break(now, if completed { "break_completed" } else { "break_skipped" });
            }
        }
        TimerEvent::PreBreakStarted => {
            notify_user(app, "30 秒后开始休息");
            let _ = app.emit("timer-event", "prebreak");
        }
        TimerEvent::PausedChanged { paused } => {
            let _ = app.emit("timer-event", serde_json::json!({"paused": paused}).to_string());
        }
    }
}

// ---------- 工具 ----------

fn display_ids(state: &State<'_, AppState>) -> Vec<String> {
    // 用枚举缓存（known_displays），避免首次运行时 current_targets 为空导致 claims 不生效
    state
        .display
        .known_displays()
        .iter()
        .map(|id| id.0.clone())
        .collect()
}

/// 预设表（设计附录 A）。
fn preset_params(preset: &str, cfg: &AppConfig) -> (u32, f64) {
    match preset {
        "health" => (4500, 0.85),
        "normal" => (6500, 1.00),
        "smart" => {
            // 智能 = DayNight 当前值；关闭时回落 health
            if cfg.display.day_night.enabled {
                let k = eyescare_core::display::day_night::day_night_kelvin(
                    &cfg.display.day_night,
                    eyescare_core::display::day_night::now_local(),
                );
                (k, 0.85)
            } else {
                (4500, 0.85)
            }
        }
        "office" => (5500, 0.90),
        "gaming" => (6500, 1.00),
        "night" => (3400, 0.70),
        "editing" => (6500, 0.95),
        "reading" => (4000, 0.80),
        _ => (4500, 0.85),
    }
}

fn preset_params_current(state: &State<'_, AppState>) -> (u32, f64) {
    let cfg = state.config.lock().unwrap();
    preset_params(&cfg.display.preset, &cfg)
}

fn refresh_engine(state: &State<'_, AppState>) {
    let rules = state.rules.lock().unwrap().clone();
    *state.engine.lock().unwrap() = RuleEngine::from_config(&rules);
}

fn reload_config_into_state(app: &AppHandle, state: &State<'_, AppState>) -> Result<(), String> {
    let cfg = state.store.load_config().map_err(|e| e.to_string())?;
    let rules = state.store.load_rules().map_err(|e| e.to_string())?;
    *lock_mutex(&state.config) = cfg.clone();
    *lock_mutex(&state.rules) = rules;
    lock_mutex(&state.timer).apply_config(cfg.timer.clone());
    *lock_mutex(&state.safe_mode) = SafeModeController::new(cfg.safe_mode.clone());
    refresh_engine(state);
    *lock_mutex(&state.last_claim_sig) = None;
    state
        .display
        .set_hdr_skip(matches!(cfg.display.hdr_policy, eyescare_core::config::HdrPolicy::Skip));
    if state.filter_enabled.load(Ordering::Relaxed) {
        reapply_display_sources(state);
    }
    if let Err(e) = register_shortcuts(app, &cfg.shortcuts) {
        tracing::warn!("re-register shortcuts after import failed: {e}");
    }
    emit_status(app, state);
    Ok(())
}

fn set_filter(app: &AppHandle, state: &State<'_, AppState>, enabled: bool) {
    state.filter_enabled.store(enabled, Ordering::Relaxed);
    if enabled {
        state.display.set_applies_enabled(true);
        reapply_display_sources(state);
    } else {
        *lock_mutex(&state.last_daynight_kelvin) = None;
        let _ = state.display.restore_all();
    }
    emit_status(app, state);
}

#[cfg(target_os = "windows")]
static SESSION_FILTER_WAS_ON: AtomicBool = AtomicBool::new(false);

/// 关机查询：只还原 gamma，进程仍在。取消关机时靠 abort_session_end 恢复滤镜。
#[cfg(target_os = "windows")]
fn prepare_session_end(app: &AppHandle) {
    if let Some(state) = app.try_state::<AppState>() {
        let on = state.filter_enabled.load(Ordering::Relaxed);
        SESSION_FILTER_WAS_ON.store(on, Ordering::Relaxed);
        state.filter_enabled.store(false, Ordering::Relaxed);
        *lock_mutex(&state.last_daynight_kelvin) = None;
        let _ = state.display.restore_all();
    }
}

#[cfg(target_os = "windows")]
fn abort_session_end(app: &AppHandle) {
    if !SESSION_FILTER_WAS_ON.swap(false, Ordering::Relaxed) {
        return;
    }
    if let Some(state) = app.try_state::<AppState>() {
        state.filter_enabled.store(true, Ordering::Relaxed);
        reapply_display_sources(&state);
    }
}

/// 退出：先关滤镜挡住 tick 回写，再标 quitting，再还原 gamma。
fn begin_quit(app: &AppHandle) {
    if let Some(state) = app.try_state::<AppState>() {
        state.filter_enabled.store(false, Ordering::Relaxed);
        *lock_mutex(&state.last_daynight_kelvin) = None;
        state.quitting.store(true, Ordering::Relaxed);
        let _ = state.display.restore_all();
    }
    app.exit(0);
}

fn reapply_display_sources(state: &State<'_, AppState>) {
    state.display.set_applies_enabled(true);
    let ids = display_ids(state);
    let cfg = lock_mutex(&state.config);
    let (k, b) = preset_params(&cfg.display.preset, &cfg);
    let default_claims: Vec<(String, Claim)> = ids
        .iter()
        .map(|id| (id.clone(), Claim::default_preset(k, b)))
        .collect();
    state.display.set_source("default", default_claims);
    if cfg.display.manual_lock {
        let user: Vec<(String, Claim)> = ids
            .iter()
            .map(|id| {
                (
                    id.clone(),
                    Claim::user_lock(cfg.display.kelvin, cfg.display.brightness),
                )
            })
            .collect();
        state.display.set_source("user", user);
    } else {
        state.display.remove_source("user");
    }
    drop(cfg);
    apply_safe_claims(state);
    update_daynight_claims(state);
    *lock_mutex(&state.last_claim_sig) = None;
    let _ = state.display.recompute();
}

fn emit_status(app: &AppHandle, state: &State<'_, AppState>) {
    let _ = app.emit("status-changed", status_json(state));
}

// ---------- 托盘 ----------

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem};

    let toggle_filter = MenuItem::with_id(app, "toggle_filter", "滤镜开关", true, None::<&str>)?;
    let safe_mode = MenuItem::with_id(app, "safe_mode", "滤镜旁路", true, None::<&str>)?;
    let start_break = MenuItem::with_id(app, "start_break", "开始引导休息", true, None::<&str>)?;
    let end_break = MenuItem::with_id(app, "end_break", "结束休息", true, None::<&str>)?;
    let preset = MenuItem::with_id(app, "preset", "预设：健康", true, None::<&str>)?;
    let today = MenuItem::with_id(app, "today", "今日洞察…", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "设置…", true, None::<&str>)?;
    let restore = MenuItem::with_id(app, "restore", "恢复显示", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &toggle_filter,
            &safe_mode,
            &start_break,
            &end_break,
            &preset,
            &today,
            &settings,
            &restore,
            &quit,
        ],
    )?;

    // 只建一个托盘。tauri.conf.json 里的 app.trayIcon 会再建一个无菜单图标。
    let icon = tauri::include_image!("icons/icon.png");
    if let Some(existing) = app.tray_by_id("main") {
        existing.set_menu(Some(menu))?;
        existing.on_menu_event(on_tray_menu);
        existing.on_tray_icon_event(on_tray_click);
        let _ = existing.set_tooltip(Some("EyesCare"));
        let _ = existing.set_icon(Some(icon));
        return Ok(());
    }

    let _tray = tauri::tray::TrayIconBuilder::with_id("main")
        .icon(icon)
        .tooltip("EyesCare")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(on_tray_menu)
        .on_tray_icon_event(on_tray_click)
        .build(app)?;

    Ok(())
}

fn on_tray_menu(app: &AppHandle, event: tauri::menu::MenuEvent) {
    match event.id.as_ref() {
        "toggle_filter" => {
            let state = app.state::<AppState>();
            let on = !state.filter_enabled.load(Ordering::Relaxed);
            set_filter(app, &state, on);
        }
        "safe_mode" => {
            let state = app.state::<AppState>();
            {
                let mut sm = lock_mutex(&state.safe_mode);
                if sm.is_active() {
                    sm.user_exit();
                } else {
                    sm.user_enter();
                }
            }
            if state.filter_enabled.load(Ordering::Relaxed) {
                apply_safe_claims(&state);
            }
            emit_status(app, &state);
        }
        "preset" => {
            let state = app.state::<AppState>();
            if let Err(e) = apply_preset(&state, "health".into()) {
                tracing::warn!("tray preset failed: {e}");
            }
        }
        "start_break" => {
            let state = app.state::<AppState>();
            start_user_guided_break(app, &state);
        }
        "end_break" => {
            let state = app.state::<AppState>();
            end_break_from_ui(app, &state);
        }
        "settings" | "today" => {
            show_settings(app);
        }
        "restore" => {
            let state = app.state::<AppState>();
            set_filter(app, &state, false);
        }
        "quit" => begin_quit(app),
        _ => {}
    }
}

fn on_tray_click(tray: &tauri::tray::TrayIcon, event: tauri::tray::TrayIconEvent) {
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
    if let TrayIconEvent::Click {
        button: MouseButton::Left,
        button_state: MouseButtonState::Up,
        ..
    } = event
    {
        show_settings(tray.app_handle());
    }
}

fn show_settings(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.set_skip_taskbar(false);
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        return;
    }
    // 先隐藏再建窗：WebView2 默认白底，内容未就绪时会白屏 1–2s。
    // 前端首帧后再 show；关闭只隐藏，下次打开可复用。
    let builder = tauri::WebviewWindowBuilder::new(
        app,
        "settings",
        tauri::WebviewUrl::App("index.html".into()),
    )
    .title("EyesCare 设置")
    .inner_size(960.0, 680.0)
    .min_inner_size(720.0, 480.0)
    .resizable(true)
    .visible(false)
    .background_color(tauri::window::Color(12, 11, 9, 255))
    .skip_taskbar(false);
    #[cfg(target_os = "windows")]
    let builder = builder.additional_browser_args(
        "--disable-background-networking --disable-features=Translate,msSmartScreenProtection --js-flags=--max-old-space-size=64",
    );
    match builder.build() {
        Ok(window) => {
            if let Some(icon) = app.default_window_icon() {
                let _ = window.set_icon(icon.clone());
            }
            let hidden = window.clone();
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = hidden.hide();
                }
            });
        }
        Err(e) => tracing::error!("failed to open settings window: {e}"),
    }
}

fn place_overlay_on_active_monitor(app: &AppHandle, window: &tauri::WebviewWindow) {
    let monitor = app
        .cursor_position()
        .ok()
        .and_then(|p| app.monitor_from_point(p.x, p.y).ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else {
        return;
    };
    let mpos = monitor.position();
    let msize = monitor.size();
    let wsize = window
        .outer_size()
        .unwrap_or(tauri::PhysicalSize::new(520, 520));
    let x = mpos.x + (msize.width as i32 - wsize.width as i32) / 2;
    let y = mpos.y + (msize.height as i32 - wsize.height as i32) / 2;
    let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
}

fn reveal_break_overlay(app: &AppHandle, window: &tauri::WebviewWindow) {
    place_overlay_on_active_monitor(app, window);
    let _ = window.set_always_on_top(true);
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
}

fn show_break_overlay(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("break") {
        reveal_break_overlay(app, &w);
        return;
    }
    let builder = tauri::WebviewWindowBuilder::new(
        app,
        "break",
        tauri::WebviewUrl::App("index.html#break".into()),
    )
    .title("EyesCare 引导休息")
    .inner_size(520.0, 520.0)
    .resizable(false)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .visible(true)
    .focused(true)
    .center()
    .background_color(tauri::window::Color(12, 11, 9, 255));
    #[cfg(target_os = "windows")]
    let builder = builder.additional_browser_args(
        "--disable-background-networking --disable-features=Translate,msSmartScreenProtection --js-flags=--max-old-space-size=64",
    );
    match builder.build() {
        Ok(window) => {
            if let Some(icon) = app.default_window_icon() {
                let _ = window.set_icon(icon.clone());
            }
            reveal_break_overlay(app, &window);
            let app_handle = app.clone();
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    if let Some(state) = app_handle.try_state::<AppState>() {
                        end_break_from_ui(&app_handle, &state);
                    }
                }
            });
        }
        Err(e) => tracing::error!("failed to open break overlay: {e}"),
    }
}

fn close_break_overlay(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("break") {
        let _ = w.hide();
    }
}

/// 开始菜单快捷方式带上 AUMID，免安装 toast 才出得来。
#[cfg(target_os = "windows")]
fn ensure_toast_shortcut() {
    use windows::core::{Interface, HSTRING};
    use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
        IPersistFile,
    };
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("current_exe for toast shortcut failed: {e}");
            return;
        }
    };
    let Some(appdata) = std::env::var_os("APPDATA") else {
        tracing::warn!("APPDATA missing; cannot create toast shortcut");
        return;
    };
    let dir = std::path::PathBuf::from(appdata)
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("create Start Menu Programs failed: {e}");
        return;
    }
    let link_path = dir.join("EyesCare.lnk");

    let result = (|| -> windows::core::Result<()> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let sl: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            sl.SetPath(&HSTRING::from(exe.to_string_lossy().as_ref()))?;
            if let Some(parent) = exe.parent() {
                let _ = sl.SetWorkingDirectory(&HSTRING::from(parent.to_string_lossy().as_ref()));
            }
            let _ = sl.SetDescription(&HSTRING::from("EyesCare"));
            let store: IPropertyStore = sl.cast()?;
            let pv = windows::core::PROPVARIANT::from("com.eyescare.app");
            store.SetValue(&PKEY_AppUserModel_ID, &pv)?;
            store.Commit()?;
            let persist: IPersistFile = sl.cast()?;
            persist.Save(&HSTRING::from(link_path.to_string_lossy().as_ref()), true)?;
            Ok(())
        }
    })();
    if let Err(e) = result {
        tracing::warn!("toast AUMID shortcut failed: {e}");
    }
}

// ---------- 入口 ----------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "windows")]
    {
        // 免安装 exe 没有安装器 AUMID，不设这个 Windows 通知经常不出。
        let _ = unsafe {
            windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID(windows::core::w!(
                "com.eyescare.app"
            ))
        };
        ensure_toast_shortcut();
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_settings(app);
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .invoke_handler(tauri::generate_handler![
            get_status,
            set_display_params,
            release_manual_lock,
            set_preset,
            restore_display,
            safe_mode_enter,
            safe_mode_exit,
            safe_mode_extend,
            safe_mode_status,
            get_rules,
            save_rules,
            set_rule_enabled,
            install_templates,
            set_timer_config,
            set_day_night_config,
            set_multi_monitor,
            set_hdr_policy,
            set_shortcuts,
            get_today_summary,
            get_full_config,
            export_config,
            import_config,
            set_filter_enabled,
            skip_break,
            start_guided_break,
        ])
        // 关闭设置窗即销毁 WebView（释放 50MB+）；进程由托盘保活。
        .setup(|app| {
            let data_dir = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."));
            let store = ConfigStore::new(data_dir.join("config"));
            let cfg = store.load_config().unwrap_or_default();
            let rules_cfg = store.load_rules().unwrap_or_default();

            // Win-first 后端
            #[cfg(target_os = "windows")]
            let display_backend: Arc<dyn DisplayBackend> =
                match eyescare_platform_win::WindowsDisplayBackend::new() {
                    Ok(b) => Arc::new(b),
                    Err(e) => {
                        tracing::error!("Windows display backend failed: {e}");
                        Arc::new(NoopBackend)
                    }
                };
            #[cfg(not(target_os = "windows"))]
            let display_backend: Arc<dyn DisplayBackend> = Arc::new(NoopBackend);

            #[cfg(target_os = "windows")]
            let foreground: Arc<dyn ForegroundAppBackend> =
                Arc::new(eyescare_platform_win::WindowsForegroundBackend);
            #[cfg(not(target_os = "windows"))]
            let foreground: Arc<dyn ForegroundAppBackend> = Arc::new(NoopForeground);

            #[cfg(target_os = "windows")]
            let system: Arc<dyn SystemBackend> =
                Arc::new(eyescare_platform_win::WindowsSystemBackend);
            #[cfg(not(target_os = "windows"))]
            let system: Arc<dyn SystemBackend> = Arc::new(NoopSystem);

            let display = Arc::new(DisplayService::new(display_backend.clone()));
            let _ = display.refresh_displays();
            display.set_hdr_skip(matches!(
                cfg.display.hdr_policy,
                eyescare_core::config::HdrPolicy::Skip
            ));
            DisplayService::install_panic_restore(&display);

            let insights = InsightsStore::open(
                &data_dir.join("insights.db"),
                cfg.insights.store_app_display_names,
            )
            .ok();
            if let Some(db) = &insights {
                let _ = db.purge(cfg.insights.retention_days.max(30));
            }

            let engine = RuleEngine::from_config(&rules_cfg);
            let state = AppState {
                display: display.clone(),
                safe_mode: Mutex::new(SafeModeController::new(cfg.safe_mode.clone())),
                timer: Mutex::new(TimerService::new(cfg.timer.clone())),
                config: Mutex::new(cfg),
                rules: Mutex::new(rules_cfg),
                store,
                insights: Mutex::new(insights),
                engine: Mutex::new(engine),
                scene: Mutex::new(None),
                last_scene_key: Mutex::new(None),
                breaks_policy: Mutex::new(BreaksPolicy::Keep),
                guided: Mutex::new(None),
                system,
                foreground,
                last_daynight_kelvin: Mutex::new(None),
                heartbeat_acc: Mutex::new(0),
                last_purge_day: Mutex::new(String::new()),
                filter_enabled: AtomicBool::new(true),
                quitting: AtomicBool::new(false),
                last_claim_sig: Mutex::new(None),
            };
            app.manage(state);
            let shortcuts = app.state::<AppState>().config.lock().unwrap().shortcuts.clone();
            if let Err(e) = register_shortcuts(app.handle(), &shortcuts) {
                tracing::warn!("global shortcut registration failed: {e}");
            }

            // 初始 claims：默认 + 手动锁（若有）+ 旁路 + DayNight
            {
                let st = app.state::<AppState>();
                reapply_display_sources(&st);
            }

            #[cfg(target_os = "windows")]
            install_session_end_hook(app.handle());

            let _ = build_tray(app.handle());
            let handle = app.handle().clone();
            spawn_tick(handle);

            // 热插拔 / 唤醒后重绑输出（订阅线程常驻，句柄可丢）
            {
                let h = app.handle().clone();
                let st = app.state::<AppState>();
                let _ = st.system.on_power_and_display_events(Box::new(move |ev| {
                    if !matches!(
                        ev,
                        SystemEvent::DisplayChanged
                            | SystemEvent::PowerResumed
                            | SystemEvent::SessionUnlocked
                    ) {
                        return;
                    }
                    if let Some(state) = h.try_state::<AppState>() {
                        if !state.filter_enabled.load(Ordering::Relaxed) {
                            return;
                        }
                        let _ = state.display.rebind_and_refresh();
                        *lock_mutex(&state.last_claim_sig) = None;
                        update_daynight_claims(&state);
                        apply_safe_claims(&state);
                    }
                }));
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building EyesCare")
        .run(|app_handle, event| {
            match event {
                tauri::RunEvent::ExitRequested { api, .. } => {
                    let quitting = app_handle
                        .try_state::<AppState>()
                        .map(|s| s.quitting.load(Ordering::Relaxed))
                        .unwrap_or(false);
                    if !quitting {
                        // 关掉设置窗不应退出托盘进程
                        api.prevent_exit();
                    }
                }
                tauri::RunEvent::Exit => {
                    // 进程真正拆除：无论路径都必须还原 gamma
                    if let Some(state) = app_handle.try_state::<AppState>() {
                        state.filter_enabled.store(false, Ordering::Relaxed);
                        let _ = state.display.restore_all();
                    }
                }
                _ => {}
            }
        });
}

/// 隐藏顶层窗接收注销/关机（WM_QUERYENDSESSION / WM_ENDSESSION）。
#[cfg(target_os = "windows")]
fn install_session_end_hook(app: &AppHandle) {
    use windows::core::w;
    use windows::Win32::Foundation::{HINSTANCE, HWND};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, SetWindowLongPtrW, GWLP_WNDPROC, HMENU, WINDOW_EX_STYLE, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW, WS_POPUP,
    };

    *lock_mutex(&SESSION_APP) = Some(app.clone());

    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0),
            w!("STATIC"),
            w!("EyesCareSessionHook"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            HWND::default(),
            HMENU::default(),
            HINSTANCE::default(),
            None,
        )
    };
    let hwnd = match hwnd {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("session-end hook window failed: {e}");
            return;
        }
    };
    let prev = unsafe {
        SetWindowLongPtrW(hwnd, GWLP_WNDPROC, session_wndproc as *const () as isize)
    };
    SESSION_ORIG_WNDPROC.store(prev, Ordering::Relaxed);
}

#[cfg(target_os = "windows")]
static SESSION_APP: Mutex<Option<AppHandle>> = Mutex::new(None);
#[cfg(target_os = "windows")]
static SESSION_ORIG_WNDPROC: std::sync::atomic::AtomicIsize =
    std::sync::atomic::AtomicIsize::new(0);

#[cfg(target_os = "windows")]
unsafe extern "system" fn session_wndproc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::UI::WindowsAndMessaging::{
        CallWindowProcW, DefWindowProcW, WM_ENDSESSION, WM_QUERYENDSESSION, WNDPROC,
    };

    if msg == WM_QUERYENDSESSION {
        // 查询阶段只还原 gamma；关机仍可能被取消，不能在这里 exit。
        if let Some(app) = lock_mutex(&SESSION_APP).clone() {
            prepare_session_end(&app);
        }
        return LRESULT(1);
    }
    if msg == WM_ENDSESSION {
        if wparam.0 != 0 {
            if let Some(app) = lock_mutex(&SESSION_APP).clone() {
                begin_quit(&app);
            }
        } else if let Some(app) = lock_mutex(&SESSION_APP).clone() {
            abort_session_end(&app);
        }
    }
    let prev = SESSION_ORIG_WNDPROC.load(Ordering::Relaxed);
    if prev == 0 {
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    } else {
        let func: WNDPROC = unsafe { std::mem::transmute(prev) };
        unsafe { CallWindowProcW(func, hwnd, msg, wparam, lparam) }
    }
}

// ---------- 非 Windows 占位后端 ----------

struct NoopBackend;

impl DisplayBackend for NoopBackend {
    fn list_displays(&self) -> eyescare_platform::Result<Vec<eyescare_platform::DisplayInfo>> {
        Ok(vec![])
    }
    fn apply_ramp(
        &self,
        id: &eyescare_platform::DisplayId,
        _ramp: &eyescare_platform::Ramp,
    ) -> eyescare_platform::Result<eyescare_platform::ApplyReport> {
        Ok(eyescare_platform::ApplyReport {
            display_id: id.clone(),
            outcome: eyescare_platform::ApplyOutcome::Applied,
            readback_diff: 0.0,
            readback_vs_original_diff: 0.0,
        })
    }
    fn restore(&self, _id: &eyescare_platform::DisplayId) -> eyescare_platform::Result<()> {
        Ok(())
    }
    fn restore_all(&self) -> eyescare_platform::Result<()> {
        Ok(())
    }
    fn rebind_outputs(&self) -> eyescare_platform::Result<()> {
        Ok(())
    }
    fn detect_hdr_active(
        &self,
        _id: &eyescare_platform::DisplayId,
    ) -> eyescare_platform::Result<bool> {
        Ok(false)
    }
}

struct NoopForeground;

impl ForegroundAppBackend for NoopForeground {
    fn foreground(&self) -> eyescare_platform::Result<eyescare_platform::SceneContext> {
        Ok(eyescare_platform::SceneContext {
            process_name: None,
            path_suffix: None,
            bundle_id: None,
            app_display_name: "Noop".into(),
            window_title: None,
            is_fullscreen: false,
            fullscreen_confidence: eyescare_platform::FullscreenConfidence::Unknown,
            fullscreen_kind: eyescare_platform::FullscreenKind::None,
            display_id: None,
        })
    }
}

struct NoopSystem;

impl SystemBackend for NoopSystem {
    fn seconds_since_input(&self) -> u64 {
        0
    }
    fn set_auto_start(&self, _on: bool) -> eyescare_platform::Result<()> {
        Ok(())
    }
    fn on_power_and_display_events(
        &self,
        _cb: eyescare_platform::EventCallback,
    ) -> eyescare_platform::Result<eyescare_platform::Subscription> {
        Ok(eyescare_platform::Subscription::new())
    }
}

#[cfg(test)]
mod tests {
    use super::{day_night_hhmm, day_night_valid};
    use eyescare_core::config::DayNightConfig;

    fn valid_dn() -> DayNightConfig {
        DayNightConfig {
            enabled: true,
            transition_minutes: 60,
            mode: Default::default(),
            day_start: "07:00".into(),
            night_start: "19:30".into(),
        }
    }

    #[test]
    fn day_night_valid_accepts_defaults() {
        assert!(day_night_valid(&valid_dn()).is_ok());
    }

    #[test]
    fn day_night_valid_rejects_zero_transition() {
        let mut c = valid_dn();
        c.transition_minutes = 0;
        let err = day_night_valid(&c).unwrap_err();
        assert!(err.contains("transition_minutes"));
    }

    #[test]
    fn day_night_valid_rejects_bad_hhmm() {
        let mut c = valid_dn();
        c.day_start = "25:00".into();
        assert!(day_night_valid(&c).is_err());
        c = valid_dn();
        c.day_start = "07:60".into();
        assert!(day_night_valid(&c).is_err());
        c = valid_dn();
        c.day_start = "7:30".into();
        assert!(day_night_valid(&c).is_ok());
        c = valid_dn();
        c.night_start = "ab:cd".into();
        assert!(day_night_valid(&c).is_err());
        assert!(!day_night_hhmm(""));
        assert!(day_night_hhmm("00:00"));
        assert!(day_night_hhmm("23:59"));
    }
}
