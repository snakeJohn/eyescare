// EyesCare 前端 ↔ Tauri 后端桥（Tauri v2）。
// 所有后端数据都经由 @tauri-apps/api 的 invoke（等价于 window.__TAURI__.core.invoke），
// 命令签名与 src-tauri/src/lib.rs 的 tauri::generate_handler! 注册表一一对应。
//
// 非 Tauri 环境（纯浏览器 vite dev/preview）自动降级到内置 mock 数据，
// 便于单独开发 UI；发布产物始终走真实 invoke。

import { invoke } from "@tauri-apps/api/core";
import {
  enable as autoStartEnable,
  disable as autoStartDisable,
  isEnabled as autoStartIsEnabled,
} from "@tauri-apps/plugin-autostart";

// ---------------------------------------------------------------------------
// 类型定义（与 eyescare-core serde 结构对齐，snake_case）
// ---------------------------------------------------------------------------

export type SafeState = "off" | "user_on" | "rule_on";
export type SafeSource = "user" | "rule";
export type SafeHold = "duration" | "while_match";

export interface SafeModeStatus {
  state: SafeState;
  source: SafeSource | null;
  hold: SafeHold | null;
  rule_id: string | null;
  remaining_sec: number | null;
  would_match_rule: string | null;
}

export interface TimerStatus {
  state: string; // working | paused | pre_break | break
  work_elapsed_sec: number;
  break_remaining_sec: number | null;
  prebreak_remaining_sec: number | null;
}

export interface StatusPayload {
  safe_mode: SafeModeStatus;
  timer: TimerStatus;
  display: {
    kelvin: number;
    brightness: number;
    preset: string;
    day_night_enabled: boolean;
  };
  filter_enabled?: boolean;
}

export interface TodaySummary {
  active_sec: number;
  filter_sec: number;
  safe_sec: number;
  break_prompted: number;
  break_completed: number;
  blue_load: number;
  filter_coverage: number;
  break_compliance: number;
  safe_ratio: number;
}

export type RuleField =
  | "process_name"
  | "bundle_id"
  | "path_suffix"
  | "app_display_name"
  | "is_fullscreen";
export type RuleOp = "equals" | "prefix" | "suffix" | "contains" | "glob";

export interface RuleCondition {
  field: RuleField;
  op: RuleOp;
  value: string;
  case_sensitive?: boolean;
}

export interface ConditionGroup {
  any: RuleCondition[];
  all: RuleCondition[];
}

export type RuleAction =
  | { action: "safe_mode"; hold: SafeHold; minutes?: number | null }
  | { action: "preset"; preset: string }
  | {
      action: "fullscreen_policy";
      filter: "keep" | "pause" | "gaming_preset";
      breaks: "keep" | "pause" | "notify_only";
    };

export interface Rule {
  id: string;
  name?: string | null;
  if_: ConditionGroup;
  then: RuleAction;
  enabled_default: boolean;
  /** 运行时启停（内存态；serde skip，不落盘） */
  enabled?: boolean;
}

export interface RulesConfig {
  schema_version: number;
  match_policy: "first_match_wins";
  rules: Rule[];
}

export interface DayNightConfig {
  enabled: boolean;
  transition_minutes: number;
  mode: "custom_times" | "sunset_table";
  day_start: string;
  night_start: string;
}

export interface TimerConfig {
  profile: "normal" | "twenty_twenty_twenty";
  work_sec: number;
  break_sec: number;
  idle_pause_sec: number;
  guided: boolean;
  silent_break: boolean;
}

export interface AppConfig {
  schema_version: number;
  display: {
    kelvin: number;
    brightness: number;
    preset: string;
    manual_lock: boolean;
    multi_monitor: "sync" | "per_display";
    pipeline: "gamma" | "compat";
    hdr_policy: "skip" | "force";
    day_night: DayNightConfig;
    per_display: Record<string, { kelvin: number; brightness: number }>;
  };
  safe_mode: {
    active: boolean;
    source: SafeSource | null;
    hold: SafeHold | null;
    default_minutes: number;
    suppress_rule_reenter_sec: number;
    suppress_after_user_exit_sec: number;
  };
  timer: {
    profile: "normal" | "twenty_twenty_twenty";
    work_sec: number;
    break_sec: number;
    idle_pause_sec: number;
    guided: boolean;
    silent_break: boolean;
  };
  insights: { store_app_display_names: boolean; retention_days: number };
  privacy: { telemetry: boolean };
  flags: { deep_link: boolean; chronotype: boolean };
  shortcuts: ShortcutConfig;
}

export interface ShortcutConfig {
  toggle_filter: string;
  toggle_safe_mode: string;
  start_break: string;
}

export interface ExportBundle {
  schema_version: number;
  exported_at_utc: string;
  config: AppConfig;
  rules: RulesConfig;
}

export interface ImportOutcome {
  rules_imported: number;
}

// ---------------------------------------------------------------------------
// invoke 封装
// ---------------------------------------------------------------------------

export const isTauri = (): boolean =>
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export class BackendError extends Error {}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri()) {
    return mockCall<T>(cmd, args);
  }
  try {
    return await invoke<T>(cmd, args);
  } catch (e) {
    const msg =
      typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
    throw new BackendError(msg);
  }
}

// ---------- 命令包装（与 lib.rs 命令一一对应） ----------

export const getStatus = () => call<StatusPayload>("get_status");

export const safeModeStatus = () => call<SafeModeStatus>("safe_mode_status");
export const safeModeEnter = () => call<void>("safe_mode_enter");
export const safeModeExit = () => call<void>("safe_mode_exit");
export const safeModeExtend = (minutes: number) =>
  call<void>("safe_mode_extend", { minutes });

export const setPreset = (preset: string) => call<void>("set_preset", { preset });
export const setDisplayParams = (kelvin: number, brightness: number) =>
  call<void>("set_display_params", { kelvin, brightness });
export const releaseManualLock = () => call<void>("release_manual_lock");
export const restoreDisplay = () => call<void>("restore_display");
export const setFilterEnabled = (enabled: boolean) =>
  call<void>("set_filter_enabled", { enabled });
export const skipBreak = () => call<void>("skip_break");
export const startGuidedBreak = () => call<void>("start_guided_break");

export const setTimerConfig = (config: TimerConfig) =>
  call<void>("set_timer_config", { config });
export const setDayNightConfig = (config: DayNightConfig) =>
  call<void>("set_day_night_config", { config });
export const setMultiMonitor = (mode: "sync" | "per_display") =>
  call<void>("set_multi_monitor", { mode });
export const setHdrPolicy = (policy: "skip" | "force") =>
  call<void>("set_hdr_policy", { policy });
export const setShortcuts = (shortcuts: ShortcutConfig) =>
  call<void>("set_shortcuts", { ...shortcuts });

export const setRuleEnabled = (ruleId: string, enabled: boolean) =>
  call<void>("set_rule_enabled", { ruleId, enabled });

export const getTodaySummary = () => call<TodaySummary>("get_today_summary");
export const getFullConfig = () => call<AppConfig>("get_full_config");

export const getRules = () => call<RulesConfig>("get_rules");
export const saveRules = (rules: RulesConfig) =>
  call<void>("save_rules", { rules });
export const installTemplates = () => call<number>("install_templates");

export const exportConfig = () => call<ExportBundle>("export_config");
export const importConfig = (bundle: ExportBundle) =>
  call<ImportOutcome>("import_config", { bundle });

// ---------- 开机自启（tauri-plugin-autostart） ----------

export async function getAutoStart(): Promise<boolean> {
  if (!isTauri()) return false;
  try {
    return await autoStartIsEnabled();
  } catch {
    return false;
  }
}

export async function setAutoStart(on: boolean): Promise<void> {
  if (!isTauri()) return;
  if (on) {
    await autoStartEnable();
  } else {
    await autoStartDisable();
  }
}

// ---------------------------------------------------------------------------
// 浏览器预览 mock（非 Tauri 环境，仅用于 UI 开发）
// ---------------------------------------------------------------------------

const MOCK_RULES: Rule[] = [
  {
    id: "design-figma-safe",
    name: "设计旁路 · Figma",
    if_: {
      any: [
        { field: "process_name", op: "equals", value: "Figma.exe" },
        { field: "bundle_id", op: "equals", value: "com.figma.Desktop" },
      ],
      all: [],
    },
    then: { action: "safe_mode", hold: "while_match" },
    enabled_default: false,
    enabled: false,
  },
  {
    id: "design-photoshop-safe",
    name: "设计旁路 · Photoshop",
    if_: { any: [{ field: "process_name", op: "equals", value: "Photoshop.exe" }], all: [] },
    then: { action: "safe_mode", hold: "while_match" },
    enabled_default: false,
    enabled: false,
  },
  {
    id: "coding-vscode",
    name: "编码 · VS Code",
    if_: {
      any: [
        { field: "process_name", op: "equals", value: "Code.exe" },
        { field: "bundle_id", op: "equals", value: "com.microsoft.VSCode" },
      ],
      all: [],
    },
    then: { action: "preset", preset: "editing" },
    enabled_default: true,
    enabled: true,
  },
  {
    id: "coding-jetbrains",
    name: "编码 · JetBrains",
    if_: {
      any: [
        { field: "process_name", op: "glob", value: "idea64.exe" },
        { field: "process_name", op: "glob", value: "webstorm64.exe" },
        { field: "process_name", op: "glob", value: "pycharm64.exe" },
      ],
      all: [],
    },
    then: { action: "preset", preset: "editing" },
    enabled_default: true,
    enabled: true,
  },
  {
    id: "gaming-fullscreen",
    name: "游戏 · 全屏",
    if_: { any: [{ field: "is_fullscreen", op: "equals", value: "true" }], all: [] },
    then: { action: "fullscreen_policy", filter: "pause", breaks: "notify_only" },
    enabled_default: true,
    enabled: true,
  },
];

const MOCK_CONFIG: AppConfig = {
  schema_version: 1,
  display: {
    kelvin: 4500,
    brightness: 0.85,
    preset: "health",
    manual_lock: false,
    multi_monitor: "sync",
    pipeline: "gamma",
    hdr_policy: "skip",
    day_night: {
      enabled: true,
      transition_minutes: 60,
      mode: "custom_times",
      day_start: "07:00",
      night_start: "19:30",
    },
    per_display: {},
  },
  safe_mode: {
    active: false,
    source: null,
    hold: null,
    default_minutes: 15,
    suppress_rule_reenter_sec: 30,
    suppress_after_user_exit_sec: 30,
  },
  timer: {
    profile: "twenty_twenty_twenty",
    work_sec: 1200,
    break_sec: 20,
    idle_pause_sec: 240,
    guided: true,
    silent_break: false,
  },
  insights: { store_app_display_names: false, retention_days: 90 },
  privacy: { telemetry: false },
  flags: { deep_link: false, chronotype: false },
  shortcuts: { toggle_filter: "Ctrl+Alt+F", toggle_safe_mode: "Ctrl+Alt+B", start_break: "Ctrl+Alt+R" },
};

let mockSafeActive = false;
let mockSafeRemaining = 0;
let mockTemplatesInstalled = false;

async function mockCall<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  // 模拟旁路倒计时（便于浏览器里看倒计时 UI）
  if (mockSafeActive && mockSafeRemaining > 0) {
    mockSafeRemaining -= 1;
    if (mockSafeRemaining <= 0) mockSafeActive = false;
  }
  switch (cmd) {
    case "get_status":
      return {
        safe_mode: {
          state: mockSafeActive ? "user_on" : "off",
          source: mockSafeActive ? "user" : null,
          hold: mockSafeActive ? "duration" : null,
          rule_id: null,
          remaining_sec: mockSafeActive ? mockSafeRemaining : null,
          would_match_rule: null,
        },
        timer: { state: "working", work_elapsed_sec: 421, break_remaining_sec: null, prebreak_remaining_sec: null },
        display: { kelvin: 4500, brightness: 0.85, preset: "health", day_night_enabled: true },
        filter_enabled: true,
      } as unknown as T;
    case "safe_mode_status":
      return {
        state: mockSafeActive ? "user_on" : "off",
        source: mockSafeActive ? "user" : null,
        hold: mockSafeActive ? "duration" : null,
        rule_id: null,
        remaining_sec: mockSafeActive ? mockSafeRemaining : null,
        would_match_rule: null,
      } as unknown as T;
    case "safe_mode_enter":
      mockSafeActive = true;
      mockSafeRemaining = 15 * 60;
      return undefined as unknown as T;
    case "safe_mode_exit":
      mockSafeActive = false;
      return undefined as unknown as T;
    case "safe_mode_extend":
      mockSafeRemaining += (args?.minutes as number) * 60;
      return undefined as unknown as T;
    case "set_preset":
      return undefined as unknown as T;
    case "restore_display":
      return undefined as unknown as T;
    case "set_filter_enabled":
      return undefined as unknown as T;
    case "skip_break":
    case "start_guided_break":
      return undefined as unknown as T;
    case "get_full_config":
      return MOCK_CONFIG as unknown as T;
    case "set_day_night_config":
    case "set_multi_monitor":
    case "set_hdr_policy":
    case "set_shortcuts":
    case "set_timer_config":
    case "set_display_params":
    case "release_manual_lock":
      return undefined as unknown as T;
    case "get_today_summary":
      return {
        active_sec: 21420,
        filter_sec: 20120,
        safe_sec: 600,
        break_prompted: 8,
        break_completed: 6,
        blue_load: 34.2,
        filter_coverage: 0.94,
        break_compliance: 0.75,
        safe_ratio: 0.028,
      } as unknown as T;
    case "get_rules":
      return {
        schema_version: 1,
        match_policy: "first_match_wins",
        rules: mockTemplatesInstalled ? MOCK_RULES : [],
      } as unknown as T;
    case "save_rules":
      return undefined as unknown as T;
    case "install_templates":
      mockTemplatesInstalled = true;
      return MOCK_RULES.length as unknown as T;
    case "export_config":
      return {
        schema_version: 1,
        exported_at_utc: new Date().toISOString(),
        config: MOCK_CONFIG,
        rules: { schema_version: 1, match_policy: "first_match_wins", rules: MOCK_RULES },
      } as unknown as T;
    case "import_config":
      return { rules_imported: 1 } as unknown as T;
    default:
      throw new BackendError(`mock: 未实现的命令 ${cmd}`);
  }
}
