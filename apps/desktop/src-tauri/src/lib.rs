//! EyesCare Tauri 壳（MVP-A 完全体接线）。
//!
//! 职责：
//! - 命令层：显示/旁路/规则/计时/昼夜/洞察/导入导出（全部持久化）
//! - 主循环（1s）：display pump、SafeMode tick、Timer tick（idle 暂停）、
//!   Scene 采样（750ms）→ RuleEngine 决策 → SafeMode/claims/breaks 联动、
//!   DayNight 计算、Insights heartbeat/rollup
//! - 崩溃/退出恢复：panic hook + CloseRequested restore_all
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
use eyescare_core::timer::{TimerEvent, TimerService};
use eyescare_platform::{DisplayBackend, ForegroundAppBackend, SystemBackend, SystemEvent};
use tauri::{AppHandle, Emitter, Manager, State};

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
    let sm = state.safe_mode.lock().unwrap().status();
    let timer = state.timer.lock().unwrap().status();
    let cfg = state.config.lock().unwrap();
    let scene = state.scene.lock().unwrap().clone();
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
fn set_timer_config(state: State<'_, AppState>, config: TimerConfig) -> Result<(), String> {
    // 校验边界（与 config.validated 一致；直接构造不经过 validated）
    if config.work_sec < 30 || config.break_sec < 5 || config.idle_pause_sec < 10 {
        return Err("invalid timer config: work_sec>=30, break_sec>=5, idle_pause_sec>=10".into());
    }
    {
        let mut cfg = state.config.lock().unwrap();
        cfg.timer = config.clone();
        state.store.save_config(&cfg).map_err(|e| e.to_string())?;
    }
    *state.timer.lock().unwrap() = TimerService::new(config);
    Ok(())
}

#[tauri::command]
fn set_day_night_config(state: State<'_, AppState>, config: DayNightConfig) -> Result<(), String> {
    {
        let mut cfg = state.config.lock().unwrap();
        cfg.display.day_night = config;
        state.store.save_config(&cfg).map_err(|e| e.to_string())?;
    }
    // 立即重算昼夜
    update_daynight_claims(&state);
    let _ = state.display.recompute();
    Ok(())
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
fn import_config(state: State<'_, AppState>, bundle: ExportBundle) -> Result<ImportOutcome, String> {
    let outcome = state.store.import(&bundle).map_err(|e| e.to_string())?;
    reload_config_into_state(&state)?;
    Ok(outcome)
}

#[tauri::command]
fn skip_break(app: AppHandle, state: State<'_, AppState>) {
    lock_mutex(&state.timer).skip();
    *lock_mutex(&state.guided) = None;
    emit_status(&app, &state);
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

    // 4) DayNight（30s 节拍）
    {
        let mut acc = state.heartbeat_acc.lock().unwrap();
        if *acc % 30 == 0 {
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
        handle_timer_event(app, &state, ev);
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
        let _ = state.display.recompute();
    }

    *lock_mutex(&state.breaks_policy) = decision.breaks_policy;
}

/// SafeMode P1 claims 同步到 DisplayService。
fn apply_safe_claims(state: &State<'_, AppState>) {
    let sm = lock_mutex(&state.safe_mode);
    let ids = display_ids(state);
    let claims = sm.p1_claims(&ids);
    if claims.is_empty() {
        state.display.remove_source("safe");
    } else {
        state.display.set_source("safe", claims);
    }
    let _ = state.display.recompute();
}

/// DayNight P4 claim 更新（目标变化才 recompute）。
fn update_daynight_claims(state: &State<'_, AppState>) {
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
    let mut last = state.last_daynight_kelvin.lock().unwrap();
    if *last != Some(k) {
        *last = Some(k);
        let ids = display_ids(state);
        let claims: Vec<(String, Claim)> = ids
            .iter()
            .map(|id| (id.clone(), Claim::day_night(k)))
            .collect();
        state.display.set_source("daynight", claims);
        let _ = state.display.recompute();
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

fn handle_timer_event(app: &AppHandle, state: &State<'_, AppState>, ev: TimerEvent) {
    match ev {
        TimerEvent::BreakDue => {
            let _ = app.emit("timer-event", "break_due");
            use tauri_plugin_notification::NotificationExt;
            let _ = app.notification().builder().title("EyesCare").body("休息时间到了，起来活动一下").show();
            if let Some(db) = lock_mutex(&state.insights).as_ref() {
                let now = chrono::Utc::now().timestamp();
                let _ = db.record_break(now, "break_prompted");
            }
        }
        TimerEvent::BreakStarted => {
            // 引导休息播放器
            let cfg = state.config.lock().unwrap();
            let silent = cfg.timer.silent_break || !cfg.timer.guided;
            let profile = cfg.timer.profile;
            let break_sec = cfg.timer.break_sec;
            drop(cfg);
            if silent {
                use tauri_plugin_notification::NotificationExt;
                let _ = app.notification().builder().title("EyesCare").body("休息开始：远眺 20 秒").show();
            } else {
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
                *state.guided.lock().unwrap() = Some(player);
            }
            let _ = app.emit("timer-event", "break_started");
        }
        TimerEvent::BreakFinished { completed } => {
            *state.guided.lock().unwrap() = None;
            let _ = app.emit("timer-event", serde_json::json!({"break_finished": completed}).to_string());
            // 洞察：休息完成/跳过
            if let Some(db) = state.insights.lock().unwrap().as_ref() {
                let now = chrono::Utc::now().timestamp();
                let _ = db.record_break(now, if completed { "break_completed" } else { "break_skipped" });
            }
        }
        TimerEvent::PreBreakStarted => {
            use tauri_plugin_notification::NotificationExt;
            let _ = app.notification().builder().title("EyesCare").body("30 秒后开始休息").show();
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

fn reload_config_into_state(state: &State<'_, AppState>) -> Result<(), String> {
    let cfg = state.store.load_config().map_err(|e| e.to_string())?;
    let rules = state.store.load_rules().map_err(|e| e.to_string())?;
    *lock_mutex(&state.config) = cfg.clone();
    *lock_mutex(&state.rules) = rules;
    *lock_mutex(&state.timer) = TimerService::new(cfg.timer.clone());
    *lock_mutex(&state.safe_mode) = SafeModeController::new(cfg.safe_mode.clone());
    refresh_engine(state);
    *lock_mutex(&state.last_claim_sig) = None;
    state
        .display
        .set_hdr_skip(matches!(cfg.display.hdr_policy, eyescare_core::config::HdrPolicy::Skip));
    if state.filter_enabled.load(Ordering::Relaxed) {
        reapply_display_sources(state);
    }
    Ok(())
}

fn set_filter(app: &AppHandle, state: &State<'_, AppState>, enabled: bool) {
    state.filter_enabled.store(enabled, Ordering::Relaxed);
    if enabled {
        reapply_display_sources(state);
    } else {
        let _ = state.display.restore_all();
    }
    emit_status(app, state);
}

fn reapply_display_sources(state: &State<'_, AppState>) {
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
            &preset,
            &today,
            &settings,
            &restore,
            &quit,
        ],
    )?;

    let _tray = tauri::tray::TrayIconBuilder::new()
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
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
            "settings" | "today" => {
                show_settings(app);
            }
            "restore" => {
                let state = app.state::<AppState>();
                set_filter(app, &state, false);
            }
            "quit" => {
                let state = app.state::<AppState>();
                state.quitting.store(true, Ordering::Relaxed);
                let _ = state.display.restore_all();
                app.exit(0);
            }
            _ => {}
        })
        .build(app)?;

    Ok(())
}

fn show_settings(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        return;
    }
    let builder = tauri::WebviewWindowBuilder::new(
        app,
        "settings",
        tauri::WebviewUrl::App("index.html".into()),
    )
    .title("EyesCare 设置")
    .inner_size(960.0, 680.0)
    .min_inner_size(720.0, 480.0)
    .resizable(true)
    .visible(true);
    #[cfg(target_os = "windows")]
    let builder = builder.additional_browser_args(
        "--disable-background-networking --disable-features=Translate,msSmartScreenProtection --js-flags=--max-old-space-size=64",
    );
    if let Err(e) = builder.build() {
        tracing::error!("failed to open settings window: {e}");
    }
}

// ---------- 入口 ----------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_settings(app);
        }))
        .plugin(tauri_plugin_notification::init())
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
            get_today_summary,
            get_full_config,
            export_config,
            import_config,
            set_filter_enabled,
            skip_break,
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

            // 初始 claims：默认预设 + DayNight
            {
                let st = app.state::<AppState>();
                if let Ok(displays) = display_backend.list_displays() {
                    let ids: Vec<String> = displays.iter().map(|d| d.id.0.clone()).collect();
                    let cfg = st.config.lock().unwrap();
                    let (k, b) = preset_params(&cfg.display.preset, &cfg);
                    let claims: Vec<(String, Claim)> = ids
                        .iter()
                        .map(|id| (id.clone(), Claim::default_preset(k, b)))
                        .collect();
                    st.display.set_source("default", claims);
                }
                update_daynight_claims(&st);
                let _ = st.display.recompute();
            }

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
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                let quitting = app_handle
                    .try_state::<AppState>()
                    .map(|s| s.quitting.load(Ordering::Relaxed))
                    .unwrap_or(false);
                if !quitting {
                    // 关掉设置窗不应退出托盘进程
                    api.prevent_exit();
                    return;
                }
                if let Some(state) = app_handle.try_state::<AppState>() {
                    let _ = state.display.restore_all();
                }
            }
        });
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
