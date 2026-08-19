// EyesCare 设置窗（960×680）：8 Tab 前端骨架。
// 差异化能力（滤镜旁路 / 场景规则 / 引导休息 / 本地洞察）均为一等公民入口。
// 所有后端交互走 src/api.ts 的 invoke 封装。

import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import * as api from "./api";
import type {
  AppConfig,
  ExportBundle,
  Rule,
  RuleAction,
  RuleCondition,
  RulesConfig,
  SafeModeStatus,
  StatusPayload,
  TodaySummary,
} from "./api";
import { Field, Note, Section, StatCard, Switch, useToast } from "./ui/kit";
import {
  BrandMark,
  IconAbout,
  IconBreaks,
  IconDisplay,
  IconGeneral,
  IconInsights,
  IconMoon,
  IconRhythm,
  IconRules,
  IconShortcuts,
  IconSun,
} from "./ui/icons";

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

const PRESETS: { id: string; name: string; kelvin: number; brightness: number }[] = [
  { id: "health", name: "健康", kelvin: 4500, brightness: 0.85 },
  { id: "normal", name: "普通", kelvin: 6500, brightness: 1.0 },
  { id: "office", name: "办公", kelvin: 5500, brightness: 0.9 },
  { id: "gaming", name: "游戏", kelvin: 6500, brightness: 1.0 },
  { id: "night", name: "夜间", kelvin: 3400, brightness: 0.7 },
  { id: "editing", name: "编辑", kelvin: 6500, brightness: 0.95 },
  { id: "reading", name: "阅读", kelvin: 4000, brightness: 0.8 },
];

const PRESET_NAMES: Record<string, string> = Object.fromEntries(
  PRESETS.map((p) => [p.id, p.name]),
);

const FIELD_NAMES: Record<string, string> = {
  process_name: "进程名",
  bundle_id: "Bundle ID",
  path_suffix: "路径后缀",
  app_display_name: "窗口标题",
  is_fullscreen: "全屏",
};

const OP_NAMES: Record<string, string> = {
  equals: "等于",
  prefix: "前缀为",
  suffix: "后缀为",
  contains: "包含",
  glob: "匹配",
};

type TabId =
  | "display"
  | "rules"
  | "breaks"
  | "insights"
  | "rhythm"
  | "shortcuts"
  | "general"
  | "about";

const TABS: { id: TabId; label: string; icon: (p: { className?: string }) => ReactNode }[] = [
  { id: "display", label: "显示", icon: IconDisplay },
  { id: "rules", label: "场景与规则", icon: IconRules },
  { id: "breaks", label: "计时与休息", icon: IconBreaks },
  { id: "insights", label: "洞察", icon: IconInsights },
  { id: "rhythm", label: "节律", icon: IconRhythm },
  { id: "shortcuts", label: "快捷键", icon: IconShortcuts },
  { id: "general", label: "通用", icon: IconGeneral },
  { id: "about", label: "关于", icon: IconAbout },
];

const DEFAULT_DRAFT: AppConfig = {
  schema_version: 1,
  display: {
    kelvin: 4500,
    brightness: 0.85,
    preset: "health",
    manual_lock: false,
    multi_monitor: "sync",
    pipeline: "gamma",
    hdr_policy: "skip",
    day_night: { enabled: true, transition_minutes: 60, mode: "custom_times", day_start: "07:00", night_start: "19:30" },
    per_display: {},
  },
  safe_mode: { active: false, source: null, hold: null, default_minutes: 15, suppress_rule_reenter_sec: 30, suppress_after_user_exit_sec: 30 },
  timer: { profile: "twenty_twenty_twenty", work_sec: 1200, break_sec: 20, idle_pause_sec: 240, guided: true, silent_break: false },
  insights: { store_app_display_names: false, retention_days: 90 },
  privacy: { telemetry: false },
  flags: { deep_link: false, chronotype: false },
  shortcuts: { toggle_filter: "Ctrl+Alt+F", toggle_safe_mode: "Ctrl+Alt+B", start_break: "Ctrl+Alt+R" },
};

// ---------------------------------------------------------------------------
// 工具函数
// ---------------------------------------------------------------------------

function fmtClock(sec: number | null | undefined): string {
  if (sec == null) return "—";
  const m = Math.floor(sec / 60);
  const s = sec % 60;
  return `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
}

function fmtDuration(sec: number): string {
  const h = Math.floor(sec / 3600);
  const m = Math.round((sec % 3600) / 60);
  if (h > 0) return `${h} 小时 ${m} 分`;
  return `${m} 分钟`;
}

function pct(v: number | undefined, digits = 0): string {
  if (v == null) return "—";
  return `${(v * 100).toFixed(digits)}%`;
}

function describeCondition(c: RuleCondition): string {
  return `${FIELD_NAMES[c.field] ?? c.field} ${OP_NAMES[c.op] ?? c.op} ${c.value}`;
}

function describeAction(a: RuleAction): string {
  switch (a.action) {
    case "safe_mode":
      return a.hold === "duration"
        ? `滤镜旁路 ${a.minutes ?? 15} 分钟`
        : "滤镜旁路（匹配期间）";
    case "preset":
      return `应用预设 · ${PRESET_NAMES[a.preset] ?? a.preset}`;
    case "fullscreen_policy":
      return "全屏：暂停滤镜 + 休息仅通知";
  }
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

export default function App() {
  const [tab, setTab] = useState<TabId>("display");
  const [theme, setTheme] = useState<"dark" | "light">(() =>
    localStorage.getItem("eyescare-theme") === "light" ? "light" : "dark",
  );
  const [version, setVersion] = useState("");
  // 配置导入成功后 +1：强制重挂载配置类 Tab（重新从后端加载）
  const [configRev, setConfigRev] = useState(0);

  useEffect(() => {
    api.getAppVersion().then(setVersion).catch(() => setVersion(""));
    if (!api.isTauri()) return;
    let cancelled = false;
    const reveal = () => {
      if (cancelled) return;
      import("@tauri-apps/api/webviewWindow")
        .then(({ getCurrentWebviewWindow }) => getCurrentWebviewWindow().show())
        .catch(() => {});
    };
    const id = requestAnimationFrame(() => requestAnimationFrame(reveal));
    return () => {
      cancelled = true;
      cancelAnimationFrame(id);
    };
  }, []);

  const current = TABS.find((t) => t.id === tab);
  const toggleTheme = () => {
    setTheme((currentTheme) => {
      const next = currentTheme === "dark" ? "light" : "dark";
      localStorage.setItem("eyescare-theme", next);
      return next;
    });
  };

  return (
    <div className={`theme-${theme} flex h-full`} style={{ background: "var(--bg)", color: "var(--fg)" }}>
      <aside
        className="flex w-[200px] shrink-0 flex-col border-r px-3 py-4"
        style={{ background: "var(--card)", borderColor: "var(--line)" }}
      >
        <div className="mb-6 flex items-center gap-2.5 px-1.5">
          <BrandMark className="h-9 w-9 shrink-0" />
          <div className="min-w-0">
            <div className="text-sm font-semibold tracking-wide">EyesCare</div>
            <div className="text-[10px]" style={{ color: "var(--muted)" }}>
              护眼助手{version ? ` · ${version}` : ""}
            </div>
          </div>
        </div>
        <nav className="flex flex-col gap-0.5">
          {TABS.map((t) => {
            const active = tab === t.id;
            const Glyph = t.icon;
            return (
              <button
                key={t.id}
                type="button"
                onClick={() => setTab(t.id)}
                className="flex w-full items-center gap-2.5 rounded-lg px-2.5 py-2 text-left text-[13px] transition-colors"
                style={{
                  background: active ? "color-mix(in srgb, var(--accent) 18%, transparent)" : "transparent",
                  color: active ? "var(--accent)" : "var(--muted)",
                }}
              >
                <Glyph className="h-4 w-4 shrink-0" />
                {t.label}
              </button>
            );
          })}
        </nav>
        <div className="mt-auto space-y-2 px-0.5">
          <button type="button" className="btn-ghost w-full justify-start gap-2 text-xs" onClick={toggleTheme}>
            {theme === "dark" ? <IconSun className="h-3.5 w-3.5" /> : <IconMoon className="h-3.5 w-3.5" />}
            {theme === "dark" ? "浅色外观" : "深色外观"}
          </button>
          <div className="px-1 text-[10px] leading-relaxed" style={{ color: "var(--faint)" }}>
            {api.isTauri() ? "已连接桌面后端" : "浏览器预览模式"}
          </div>
        </div>
      </aside>

      <div className="flex min-w-0 flex-1 flex-col">
        <header
          className="flex items-center justify-between border-b px-6 py-3"
          style={{ borderColor: "var(--line)" }}
        >
          <div>
            <div className="text-sm font-semibold">{current?.label}</div>
            <div className="text-[11px]" style={{ color: "var(--muted)" }}>
              荷鲁斯之眼 · 本地节律与滤镜
            </div>
          </div>
          <StatusPills />
        </header>
        <main className="min-h-0 flex-1 overflow-y-auto px-6 py-5">
          {tab === "display" && <DisplayTab />}
          {tab === "rules" && <RulesTab key={`r${configRev}`} />}
          {tab === "breaks" && <BreaksTab key={`b${configRev}`} />}
          {tab === "insights" && <InsightsTab />}
          {tab === "rhythm" && <RhythmTab key={`rh${configRev}`} />}
          {tab === "shortcuts" && <ShortcutsTab />}
          {tab === "general" && <GeneralTab onImported={() => setConfigRev((v) => v + 1)} />}
          {tab === "about" && <AboutTab version={version} />}
        </main>
      </div>
    </div>
  );
}

export function BreakOverlay() {
  const status = useStatus();
  const { push, node } = useToast();
  const theme = localStorage.getItem("eyescare-theme") === "light" ? "light" : "dark";
  const remaining = status?.timer.break_remaining_sec ?? 0;
  const title = status?.guided?.title ?? "远眺窗外";
  const body = status?.guided?.body ?? "看向 6 米以外，起身活动一下肩颈。";

  const skip = useCallback(async () => {
    try {
      await api.skipBreak();
    } catch (e) {
      push(`操作失败：${(e as Error).message}`, "err");
    }
  }, [push]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        void skip();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [skip]);

  return (
    <div
      className={`theme-${theme} flex h-full flex-col items-center justify-center px-8 text-center`}
      style={{
        background: "var(--bg)",
        color: "var(--fg)",
        border: "1px solid color-mix(in srgb, var(--accent) 35%, var(--line))",
      }}
    >
      <BrandMark className="h-16 w-16" />
      <div className="mt-5 text-[11px] tracking-[0.2em] uppercase" style={{ color: "var(--accent)" }}>
        引导休息
      </div>
      <h1 className="mt-2 text-2xl font-semibold">{title}</h1>
      <p className="mt-2 max-w-sm text-sm leading-relaxed" style={{ color: "var(--muted)" }}>
        {body}
      </p>
      <div className="mt-6 font-mono text-5xl font-semibold tabular-nums" style={{ color: "var(--accent)" }}>
        {fmtClock(remaining)}
      </div>
      <button type="button" className="btn-ghost mt-8" onClick={skip}>
        结束本次休息
      </button>
      {node}
    </div>
  );
}

function StatusPills() {
  const status = useStatus();
  const filterOn = status?.filter_enabled !== false;
  const safeOn = status?.safe_mode?.state === "user_on" || status?.safe_mode?.state === "rule_on";
  const pill = (label: string, on: boolean) => (
    <span
      className="rounded-full border px-2.5 py-0.5 text-[11px]"
      style={{
        borderColor: on ? "color-mix(in srgb, var(--accent) 45%, var(--line))" : "var(--line)",
        color: on ? "var(--accent)" : "var(--muted)",
        background: on ? "color-mix(in srgb, var(--accent) 12%, transparent)" : "transparent",
      }}
    >
      {label}
    </span>
  );
  return (
    <div className="flex items-center gap-1.5">
      {pill(filterOn ? "滤镜开" : "滤镜关", filterOn)}
      {pill(safeOn ? "旁路中" : "旁路关", safeOn)}
    </div>
  );
}

// ---------------------------------------------------------------------------
// 通用状态钩子：轮询 get_status（旁路倒计时依赖它）
// ---------------------------------------------------------------------------

// 全局单例轮询：所有 Tab 共享一个 2s 定时器；窗口隐藏时暂停（省电/省 IPC）
let sharedStatus: StatusPayload | null = null;
const sharedSubs = new Set<(s: StatusPayload | null) => void>();
let sharedTimer: number | null = null;
let eventUnlisten: (() => void) | null = null;
function ensurePolling() {
  if (sharedTimer !== null) return;
  const tick = async () => {
    if (typeof document !== "undefined" && document.hidden) return;
    try {
      const s = await api.getStatus();
      sharedStatus = s;
      sharedSubs.forEach((f) => f(s));
    } catch {
      /* 后端未就绪时静默 */
    }
  };
  tick();
  sharedTimer = window.setInterval(tick, 2000);
  if (typeof document !== "undefined") {
    document.addEventListener("visibilitychange", () => {
      if (!document.hidden) void tick();
    });
  }
  // 后端状态变化事件（旁路切换等）：即时刷新，不等下一轮询
  if (api.isTauri()) {
    import("@tauri-apps/api/event").then(({ listen }) => {
      listen<StatusPayload>("status-changed", (e) => {
        sharedStatus = e.payload;
        sharedSubs.forEach((f) => f(e.payload));
      }).then((un) => {
        eventUnlisten = un;
      });
    });
  }
}
function useStatus() {
  const [status, setStatus] = useState<StatusPayload | null>(sharedStatus);
  useEffect(() => {
    ensurePolling();
    sharedSubs.add(setStatus);
    return () => {
      sharedSubs.delete(setStatus);
    };
  }, []);
  return status;
}

// ---------------------------------------------------------------------------
// 1. 显示
// ---------------------------------------------------------------------------

function DisplayTab() {
  const status = useStatus();
  const { push, node } = useToast();

  const safe: SafeModeStatus | null = status?.safe_mode ?? null;
  const display = status?.display;
  const safeActive = safe?.state === "user_on" || safe?.state === "rule_on";
  const filterEnabled = status?.filter_enabled !== false;

  // 显示参数（后端权威；滑杆防抖写入）
  const [kelvin, setKelvin] = useState(4500);
  const [brightness, setBrightness] = useState(0.85);
  const [presetId, setPresetId] = useState<string>("health");
  const [dayNight, setDayNight] = useState(true);
  const [multi, setMulti] = useState<"sync" | "per_display">("sync");
  const [dayStart, setDayStart] = useState("07:00");
  const [nightStart, setNightStart] = useState("19:30");

  useEffect(() => {
    api
      .getFullConfig()
      .then((cfg) => {
        setDayStart(cfg.display.day_night.day_start);
        setNightStart(cfg.display.day_night.night_start);
        setMulti(cfg.display.multi_monitor);
        setDayNight(cfg.display.day_night.enabled);
      })
      .catch(() => {});
  }, []);

  // 后端状态 → 本地草稿（仅在后端值变化时同步，避免覆盖用户拖动）
  useEffect(() => {
    if (!display) return;
    setKelvin(display.kelvin);
    setBrightness(display.brightness);
    setPresetId(display.preset);
    setDayNight(display.day_night_enabled);
  }, [display?.kelvin, display?.brightness, display?.preset, display?.day_night_enabled]);

  // 防抖写入后端（500ms；拖到底停顿后落一次）
  const writeTimer = useRef<number | null>(null);
  const scheduleDisplayWrite = (k: number, b: number) => {
    if (writeTimer.current !== null) window.clearTimeout(writeTimer.current);
    writeTimer.current = window.setTimeout(async () => {
      try {
        await api.setDisplayParams(k, b);
        push(`已写入 ${k}K / ${Math.round(b * 100)}%`);
      } catch (e) {
        push(`写入失败：${(e as Error).message}`, "err");
      }
    }, 250);
  };

  // 昼夜配置防抖写入（700ms）
  const dnTimer = useRef<number | null>(null);
  const scheduleDayNightWrite = (enabled: boolean, ds: string, ns: string) => {
    if (dnTimer.current !== null) window.clearTimeout(dnTimer.current);
    dnTimer.current = window.setTimeout(async () => {
      try {
        await api.setDayNightConfig({
          enabled,
          transition_minutes: 60,
          mode: "custom_times",
          day_start: ds || "07:00",
          night_start: ns || "19:30",
        });
        push("昼夜设置已保存");
      } catch (e) {
        push(`保存失败：${(e as Error).message}`, "err");
      }
    }, 700);
  };

  const applyPreset = async (id: string) => {
    try {
      await api.setPreset(id);
      const p = PRESETS.find((x) => x.id === id);
      if (p) {
        setKelvin(p.kelvin);
        setBrightness(p.brightness);
      }
      setPresetId(id);
      if (id === "smart") setDayNight(true);
      push(`已应用预设「${PRESET_NAMES[id] ?? id}」`);
    } catch (e) {
      push(`预设失败：${(e as Error).message}`, "err");
    }
  };

  const toggleSafe = async () => {
    try {
      if (safeActive) {
        await api.safeModeExit();
        push("已退出滤镜旁路，滤镜恢复");
      } else {
        await api.safeModeEnter();
        push("滤镜旁路已开启（取色友好）");
      }
    } catch (e) {
      push(`操作失败：${(e as Error).message}`, "err");
    }
  };

  const extendSafe = async () => {
    try {
      await api.safeModeExtend(15);
      push("旁路延长 15 分钟");
    } catch (e) {
      push(`操作失败：${(e as Error).message}`, "err");
    }
  };

  const restore = async () => {
    try {
      await api.restoreDisplay();
      push("已恢复显示");
    } catch (e) {
      push(`恢复失败：${(e as Error).message}`, "err");
    }
  };

  return (
    <div className="space-y-4">
      {/* 滤镜旁路：一等公民 */}
      <section className="rounded-xl border border-brand-500/40 bg-brand-500/10 p-4 shadow-inset">
        <div className="flex items-center justify-between gap-4">
          <div>
            <h3 className="text-sm font-semibold text-brand-300">滤镜旁路 · 取色友好</h3>
            <p className="mt-1 text-xs leading-relaxed text-zinc-400">
              恢复系统 gamma，截图取色不发黄。设计取色、录屏、演示时开启。
            </p>
          </div>
          <Switch checked={safeActive} onChange={toggleSafe} />
        </div>
        {safeActive && (
          <div className="mt-3 flex flex-wrap items-center gap-3 rounded-lg bg-zinc-950/50 px-3.5 py-2.5">
            <div className="font-mono text-2xl font-semibold tabular-nums text-brand-300">
              {fmtClock(safe?.remaining_sec)}
            </div>
            <div className="text-xs text-zinc-400">
              {safe?.source === "rule" ? (
                <>由规则触发{safe.rule_id ? `（${safe.rule_id}）` : ""}</>
              ) : (
                "用户手动旁路中"
              )}
              {safe?.would_match_rule ? (
                <span className="mt-0.5 block text-[11px] text-zinc-500">
                  旁路期间规则「{safe.would_match_rule}」命中但不触发
                </span>
              ) : null}
            </div>
            <div className="ml-auto flex gap-2">
              <button type="button" className="btn-ghost" onClick={extendSafe}>
                +15 分钟
              </button>
              <button type="button" className="btn-danger" onClick={toggleSafe}>
                退出旁路
              </button>
            </div>
          </div>
        )}
      </section>

      {!filterEnabled && (
        <div className="flex items-center justify-between gap-3 rounded-xl border border-amber-900/50 bg-amber-950/30 px-3.5 py-2.5 text-xs text-amber-200">
          <span>滤镜已关闭（系统 gamma 已恢复）。规则与昼夜不会再改色温。</span>
          <button
            type="button"
            className="btn-primary text-[11px]"
            onClick={async () => {
              try {
                await api.setFilterEnabled(true);
                push("滤镜已重新开启");
              } catch (e) {
                push(`开启失败：${(e as Error).message}`, "err");
              }
            }}
          >
            重新开启
          </button>
        </div>
      )}

      {/* HDR 提示横幅（占位） */}
      <div className="flex items-start gap-2.5 rounded-xl border border-sky-900/60 bg-sky-950/30 px-3.5 py-2.5 text-xs leading-relaxed text-sky-300/90">
        <span className="mt-px">ℹ</span>
        <span>
          HDR 提示：检测到 HDR 显示器时滤镜将自动跳过（策略：跳过），避免色彩失真。
          <span className="ml-1 rounded bg-sky-900/60 px-1 py-px text-[10px] text-sky-200">占位</span>
        </span>
      </div>

      {/* 预设 */}
      <Section title="预设" desc="一键切换典型场景；「智能」跟随昼夜节律自动切换。">
        <div className="grid grid-cols-4 gap-2">
          {PRESETS.map((p) => (
            <button
              key={p.id}
              type="button"
              onClick={() => applyPreset(p.id)}
              className={`rounded-lg border px-2 py-2 text-center transition-colors ${
                presetId === p.id
                  ? "border-brand-500 bg-brand-500/15 text-brand-300"
                  : "border-surface-border bg-surface-raised text-zinc-300 hover:border-zinc-600"
              }`}
            >
              <div className="text-[13px] font-medium">{p.name}</div>
              <div className="mt-0.5 text-[10px] text-zinc-500">
                {p.kelvin}K · {Math.round(p.brightness * 100)}%
              </div>
            </button>
          ))}
          <button
            type="button"
            onClick={() => applyPreset("smart")}
            className={`rounded-lg border px-2 py-2 text-center transition-colors ${
              presetId === "smart" || dayNight
                ? "border-brand-500 bg-brand-500/15 text-brand-300"
                : "border-surface-border bg-surface-raised text-zinc-300 hover:border-zinc-600"
            }`}
          >
            <div className="text-[13px] font-medium">智能</div>
            <div className="mt-0.5 text-[10px] text-zinc-500">昼夜 · {dayNight ? "开" : "关"}</div>
          </button>
        </div>
      </Section>

      {/* 滑杆：实时写入后端（500ms 防抖） */}
      <Section
        title="微调"
        desc="拖动即写入（P2 手动锁定）。拖动色温/亮度后进入「自定义」预设；「释放锁定」恢复规则/昼夜接管。"
      >
        <div className="space-y-4">
          <Field label={`色温 ${kelvin}K`}>
            <input
              type="range"
              min={1000}
              max={10000}
              step={100}
              value={kelvin}
              onInput={(e) => {
                const k = Number(e.currentTarget.value);
                setKelvin(k);
                setPresetId("custom");
                scheduleDisplayWrite(k, brightness);
              }}
            />
            <div className="mt-1 flex justify-between text-[10px] text-zinc-600">
              <span>1000K（暖）</span>
              <span>10000K（冷）</span>
            </div>
          </Field>
          <Field label={`亮度 ${Math.round(brightness * 100)}%`}>
            <input
              type="range"
              min={1}
              max={100}
              step={1}
              value={Math.round(brightness * 100)}
              onInput={(e) => {
                const b = Number(e.currentTarget.value) / 100;
                setBrightness(b);
                setPresetId("custom");
                scheduleDisplayWrite(kelvin, b);
              }}
            />
          </Field>
          <div className="flex items-center justify-between">
            <div className="text-[11px] text-zinc-600">
              {presetId === "custom" ? "已锁定自定义参数（P2）" : "当前为预设/规则/昼夜接管"}
            </div>
            <button
              type="button"
              className="btn-ghost text-[11px]"
              onClick={async () => {
                try {
                  await api.releaseManualLock();
                  push("已释放手动锁定，恢复自动接管");
                } catch (e) {
                  push(`释放失败：${(e as Error).message}`, "err");
                }
              }}
            >
              释放锁定
            </button>
          </div>
        </div>
      </Section>

      {/* 多屏 + 昼夜 + 恢复 */}
      <div className="grid grid-cols-2 gap-4">
        <Section title="多显示器">
          <div className="space-y-2">
            {(
              [
                ["sync", "同步（Sync）", "所有显示器应用相同滤镜"],
                ["per_display", "独立（PerDisplay）", "每屏单独调整（MVP-B）"],
              ] as const
            ).map(([v, name, desc]) => (
              <label key={v} className="flex cursor-pointer items-center gap-2.5">
                <input
                  type="radio"
                  name="multi"
                  checked={multi === v}
                  onChange={() => {
                    setMulti(v);
                    api.setMultiMonitor(v).catch((e) =>
                      push(`保存失败：${(e as Error).message}`, "err"),
                    );
                  }}
                  className="accent-amber-500"
                />
                <span className="text-xs text-zinc-300">{name}</span>
                <span className="text-[10px] text-zinc-600">{desc}</span>
              </label>
            ))}
          </div>
        </Section>
        <Section title="昼夜">
          <div className="flex items-center justify-between">
            <span className="text-xs text-zinc-300">启用昼夜自动切换</span>
            <Switch
              checked={dayNight}
              onChange={(v) => {
                setDayNight(v);
                scheduleDayNightWrite(v, dayStart, nightStart);
              }}
            />
          </div>
          <div className="mt-3 grid grid-cols-2 gap-3">
            <Field label="白天开始">
              <input
                type="time"
                className="input w-full"
                value={dayStart}
                onChange={(e) => {
                  setDayStart(e.target.value);
                  scheduleDayNightWrite(dayNight, e.target.value, nightStart);
                }}
              />
            </Field>
            <Field label="夜间开始">
              <input
                type="time"
                className="input w-full"
                value={nightStart}
                onChange={(e) => {
                  setNightStart(e.target.value);
                  scheduleDayNightWrite(dayNight, dayStart, e.target.value);
                }}
              />
            </Field>
          </div>
        </Section>
      </div>

      <div className="flex items-center justify-between">
        <button type="button" className="btn-ghost" onClick={restore}>
          ↺ 恢复显示（还原系统 gamma）
        </button>
        <span className="text-[11px] text-zinc-600">当前预设：{presetId}</span>
      </div>
      {node}
    </div>
  );
}

// ---------------------------------------------------------------------------
// 2. 场景与规则
// ---------------------------------------------------------------------------

function RulesTab() {
  const [rules, setRules] = useState<RulesConfig | null>(null);
  const [busy, setBusy] = useState(false);
  const [newName, setNewName] = useState("");
  const [newProcess, setNewProcess] = useState("");
  const [newPreset, setNewPreset] = useState("editing");
  const { push, node } = useToast();

  const refresh = useCallback(async () => {
    try {
      setRules(await api.getRules());
    } catch (e) {
      push(`读取规则失败：${(e as Error).message}`, "err");
    }
  }, [push]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const install = async () => {
    setBusy(true);
    try {
      const added = await api.installTemplates();
      push(added > 0 ? `已安装 ${added} 条内置模板` : "模板已是最新（无新增）");
      await refresh();
    } catch (e) {
      push(`安装失败：${(e as Error).message}`, "err");
    } finally {
      setBusy(false);
    }
  };

  const toggleRule = async (id: string, on: boolean) => {
    if (!rules) return;
    setRules({
      ...rules,
      rules: rules.rules.map((r) => (r.id === id ? { ...r, enabled: on } : r)),
    });
    try {
      await api.setRuleEnabled(id, on); // 落盘 + 引擎热更新
    } catch (e) {
      push(`保存失败：${(e as Error).message}`, "err");
    }
  };

  const addRule = async () => {
    if (!rules || !newProcess.trim()) {
      push("请填写要匹配的进程名", "err");
      return;
    }
    const next: RulesConfig = {
      ...rules,
      rules: [...rules.rules, {
        id: `custom-${Date.now()}`, name: newName.trim() || newProcess.trim(),
        enabled_default: true, enabled: true,
        if_: { any: [{ field: "process_name", op: "equals", value: newProcess.trim() }], all: [] },
        then: { action: "preset", preset: newPreset },
      }],
    };
    try {
      await api.saveRules(next);
      setRules(next); setNewName(""); setNewProcess("");
      push("规则已添加");
    } catch (e) { push(`保存失败：${(e as Error).message}`, "err"); }
  };

  const deleteRule = async (id: string) => {
    if (!rules) return;
    const next = { ...rules, rules: rules.rules.filter((rule) => rule.id !== id) };
    try { await api.saveRules(next); setRules(next); push("规则已删除"); }
    catch (e) { push(`删除失败：${(e as Error).message}`, "err"); }
  };

  const renderActionTag = (a: RuleAction) => {
    const base = "rounded px-1.5 py-0.5 text-[10px]";
    if (a.action === "safe_mode") return <span className={`${base} bg-amber-500/15 text-amber-300`}>旁路</span>;
    if (a.action === "preset") return <span className={`${base} bg-sky-500/15 text-sky-300`}>预设</span>;
    return <span className={`${base} bg-violet-500/15 text-violet-300`}>全屏</span>;
  };

  return (
    <div className="space-y-4">
      <Section
        title="内置模板"
        desc="一键安装官方模板：设计工具旁路（默认关闭，opt-in）、编码预设、游戏全屏策略。"
      >
        <button type="button" className="btn-primary" onClick={install} disabled={busy}>
          {busy ? "安装中…" : "一键安装内置模板"}
        </button>
      </Section>

      <Section
        title={`规则列表（${rules?.rules.length ?? 0}）`}
        desc="按顺序匹配（first-match wins）。开关实时生效并持久化；新增/删除规则将在 MVP-B 提供。"
      >
        {!rules ? (
          <div className="py-6 text-center text-xs text-zinc-600">加载中…</div>
        ) : rules.rules.length === 0 ? (
          <div className="rounded-lg border border-dashed border-zinc-700 py-6 text-center text-xs text-zinc-500">
            暂无规则，点击上方按钮安装内置模板
          </div>
        ) : (
          <ul className="divide-y divide-surface-border">
            {rules.rules.map((r: Rule) => (
              <li key={r.id} className="flex items-center gap-3 py-2.5">
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <span className="text-[13px] font-medium text-zinc-200">
                      {r.name ?? r.id}
                    </span>
                    {renderActionTag(r.then)}
                    {!r.enabled_default && (
                      <span className="rounded bg-zinc-800 px-1.5 py-0.5 text-[10px] text-zinc-500">
                        opt-in
                      </span>
                    )}
                  </div>
                  <div className="mt-0.5 truncate text-[11px] text-zinc-500">
                    当 {r.if_.any.map(describeCondition).join(" 或 ")}
                    {r.if_.all.length > 0 && ` 且 ${r.if_.all.map(describeCondition).join(" 且 ")}`}
                    {" → "}
                    {describeAction(r.then)}
                  </div>
                </div>
                <Switch checked={!!r.enabled} onChange={(v) => toggleRule(r.id, v)} />
                <button type="button" className="btn-danger text-xs" onClick={() => deleteRule(r.id)}>删除</button>
              </li>
            ))}
          </ul>
        )}
      </Section>

      <Section title="新增规则" desc="匹配指定进程名后自动应用预设。">
        <div className="grid grid-cols-1 gap-3 md:grid-cols-3">
          <Field label="规则名称（可选）"><input className="input w-full" value={newName} onChange={(e) => setNewName(e.target.value)} placeholder="编码模式" /></Field>
          <Field label="进程名"><input className="input w-full" value={newProcess} onChange={(e) => setNewProcess(e.target.value)} placeholder="Code.exe" /></Field>
          <Field label="应用预设"><select className="input w-full" value={newPreset} onChange={(e) => setNewPreset(e.target.value)}>{PRESETS.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}</select></Field>
        </div>
        <button type="button" className="btn-primary mt-3" onClick={addRule}>＋ 添加规则</button>
      </Section>

      <Note>
        规则匹配依据前台应用身份（进程名 / Bundle ID / 路径后缀 / 全屏状态），全部在本地完成；
        应用显示名默认哈希落库，不离开本机。
      </Note>
      {node}
    </div>
  );
}

// ---------------------------------------------------------------------------
// 3. 计时与引导休息
// ---------------------------------------------------------------------------

function BreaksTab() {
  const status = useStatus();
  const { push, node } = useToast();
  const [profile, setProfile] = useState<"normal" | "twenty_twenty_twenty">(
    "twenty_twenty_twenty",
  );
  const [workMin, setWorkMin] = useState(20);
  const [breakVal, setBreakVal] = useState(20); // 普通=分钟，20-20-20=秒
  const [idleMin, setIdleMin] = useState(4);
  const [guided, setGuided] = useState(true);
  const [silent, setSilent] = useState(false);
  const [saved, setSaved] = useState(true);

  // 从后端加载当前计时配置（get_full_config）
  useEffect(() => {
    api
      .getFullConfig()
      .then((cfg) => {
        setProfile(cfg.timer.profile);
        setWorkMin(Math.round(cfg.timer.work_sec / 60));
        setBreakVal(
          cfg.timer.profile === "twenty_twenty_twenty"
            ? cfg.timer.break_sec
            : Math.round(cfg.timer.break_sec / 60),
        );
        setIdleMin(Math.round(cfg.timer.idle_pause_sec / 60));
        setGuided(cfg.timer.guided);
        setSilent(cfg.timer.silent_break);
        setSaved(true);
      })
      .catch(() => {});
  }, []);

  const saveTimer = async () => {
    const workSec = workMin * 60;
    const breakSec =
      profile === "twenty_twenty_twenty" ? breakVal : breakVal * 60;
    const idleSec = idleMin * 60;
    try {
      await api.setTimerConfig({
        profile,
        work_sec: workSec,
        break_sec: breakSec,
        idle_pause_sec: idleSec,
        guided,
        silent_break: silent,
      });
      setSaved(true);
      push("计时配置已保存并生效");
    } catch (e) {
      push(`保存失败：${(e as Error).message}`, "err");
    }
  };

  const timer = status?.timer;
  const timerStateName: Record<string, string> = {
    working: "专注中",
    paused: "空闲暂停",
    pre_break: "即将休息",
    break: "休息中",
  };

  return (
    <div className="space-y-4">
      <Section title="休息模式">
        <div className="grid grid-cols-2 gap-2">
          {(
            [
              ["twenty_twenty_twenty", "20-20-20", "每 20 分钟远眺并起立 20 秒"],
              ["normal", "普通", "工作 N 分钟休息 M 分钟"],
            ] as const
          ).map(([v, name, desc]) => (
            <button
              key={v}
              type="button"
              onClick={() => {
                setProfile(v);
                setSaved(false);
              }}
              className={`rounded-lg border px-3 py-2.5 text-left transition-colors ${
                profile === v
                  ? "border-brand-500 bg-brand-500/15"
                  : "border-surface-border bg-surface-raised hover:border-zinc-600"
              }`}
            >
              <div className="text-[13px] font-medium text-zinc-200">{name}</div>
              <div className="mt-0.5 text-[11px] text-zinc-500">{desc}</div>
            </button>
          ))}
        </div>
      </Section>

      <Section title="参数">
        <div className="grid grid-cols-2 gap-4">
          <Field label="工作时长（分钟）">
            <input
              type="number"
              min={1}
              className="input w-full"
              value={workMin}
              onChange={(e) => {
                setWorkMin(Math.max(1, Number(e.target.value)));
                setSaved(false);
              }}
            />
          </Field>
          <Field
            label={profile === "twenty_twenty_twenty" ? "休息时长（秒）" : "休息时长（分钟）"}
            hint={profile === "twenty_twenty_twenty" ? "建议 20 秒：远眺 6 米 + 起立活动" : undefined}
          >
            <input
              type="number"
              min={profile === "twenty_twenty_twenty" ? 5 : 1}
              className="input w-full"
              value={breakVal}
              onChange={(e) => {
                const floor = profile === "twenty_twenty_twenty" ? 5 : 1;
                setBreakVal(Math.max(floor, Number(e.target.value)));
                setSaved(false);
              }}
            />
          </Field>
          <Field label="空闲暂停阈值（分钟）" hint="无操作超过阈值即暂停计时，恢复操作后继续">
            <input
              type="number"
              min={1}
              className="input w-full"
              value={idleMin}
              onChange={(e) => {
                setIdleMin(Math.max(1, Number(e.target.value)));
                setSaved(false);
              }}
            />
          </Field>
          <div className="flex items-end">
            <div className="w-full rounded-lg border border-surface-border bg-surface-raised px-3 py-2 text-xs text-zinc-400">
              {timer ? (
                <>
                  当前状态：
                  <span className="ml-1 font-medium text-brand-300">
                    {timerStateName[timer.state] ?? timer.state}
                  </span>
                  <span className="ml-2 text-[11px] text-zinc-600">
                    本次专注 {fmtDuration(timer.work_elapsed_sec)}
                  </span>
                </>
              ) : (
                "计时器状态加载中…"
              )}
            </div>
          </div>
        </div>
      </Section>

      <Section title="引导与通知">
        <div className="space-y-3">
          <div className="flex items-center justify-between">
            <div>
              <div className="text-xs text-zinc-300">引导休息</div>
              <div className="text-[11px] text-zinc-600">到点弹出远眺与起立引导。关闭后仅通知；快捷键和按钮仍可弹出</div>
            </div>
            <Switch
              checked={guided}
              onChange={(v) => {
                setGuided(v);
                setSaved(false);
              }}
            />
          </div>
          <div className="flex items-center justify-between">
            <div>
              <div className="text-xs text-zinc-300">静默休息</div>
              <div className="text-[11px] text-zinc-600">自动到点不弹窗，仅通知。手动开始仍会弹出引导</div>
            </div>
            <Switch
              checked={silent}
              onChange={(v) => {
                setSilent(v);
                setSaved(false);
              }}
            />
          </div>
        </div>
      </Section>

      <div className="flex items-center gap-3">
        <button type="button" className="btn-primary" onClick={saveTimer} disabled={saved}>
          {saved ? "配置已生效" : "保存并生效"}
        </button>
        <button
          type="button"
          className="btn-ghost"
          onClick={async () => {
            try {
              await api.startGuidedBreak();
              push("已开始引导休息");
            } catch (e) {
              push(`开始失败：${(e as Error).message}`, "err");
            }
          }}
        >
          立即开始引导休息
        </button>
        {timer?.state === "break" && (
          <button
            type="button"
            className="btn-ghost"
            onClick={async () => {
              try {
                await api.skipBreak();
                push("已跳过本次休息");
              } catch (e) {
                push(`操作失败：${(e as Error).message}`, "err");
              }
            }}
          >
            跳过本次休息
          </button>
        )}
        {!saved && <span className="text-[11px] text-amber-300">有未保存的修改</span>}
      </div>

      <Note>
        计时参数与引导选项写入本地 config.json 并立即生效，不会清空当前专注进度。
        空闲暂停依赖系统输入检测（无操作超过阈值即暂停计时）。
      </Note>
      {node}
    </div>
  );
}

// ---------------------------------------------------------------------------
// 4. 洞察
// ---------------------------------------------------------------------------

function InsightsTab() {
  const [summary, setSummary] = useState<TodaySummary | null>(null);
  const { push, node } = useToast();

  useEffect(() => {
    api
      .getTodaySummary()
      .then(setSummary)
      .catch((e) => push(`读取洞察失败：${(e as Error).message}`, "err"));
  }, [push]);

  const s = summary;
  return (
    <div className="space-y-4">
      <Section title="今日指标" desc="全部数据在本地计算与存储，不上传任何内容。">
        {!s ? (
          <div className="py-8 text-center text-xs text-zinc-600">加载中…</div>
        ) : (
          <>
            <div className="grid grid-cols-2 gap-3 lg:grid-cols-3">
              <StatCard label="活跃时长" value={fmtDuration(s.active_sec)} />
              <StatCard
                label="滤镜覆盖率"
                value={pct(s.filter_coverage, 1)}
                sub={`滤镜开启 ${fmtDuration(s.filter_sec)}`}
                accent
              />
              <StatCard
                label="休息遵从率"
                value={pct(s.break_compliance, 1)}
                sub={`完成 ${s.break_completed}/${s.break_prompted} 次引导休息`}
              />
              <StatCard
                label="蓝光相对负荷"
                value={`${Math.round(s.blue_load)}/100`}
                sub="相对分，非光度计量"
                accent
              />
              <StatCard
                label="旁路时长占比"
                value={pct(s.safe_ratio, 1)}
                sub={`旁路 ${fmtDuration(s.safe_sec)}`}
              />
              <div className="flex items-center justify-center rounded-xl border border-dashed border-zinc-800 text-[11px] text-zinc-600">
                历史趋势 · MVP-B
              </div>
            </div>
          </>
        )}
      </Section>

      <Note>
        以上指标仅供自我管理参考，不构成医疗建议。若出现持续眼疲劳、干涩或视力变化，请咨询眼科医生。
      </Note>
      <Note>数据保留 90 天，到期自动清理；应用显示名默认哈希化存储，明文显示名默认关闭。</Note>
      {node}
    </div>
  );
}

// ---------------------------------------------------------------------------
// 5. 节律
// ---------------------------------------------------------------------------

function RhythmTab() {
  const [enabled, setEnabled] = useState(true);
  const [transition, setTransition] = useState(60);
  const [dayStart, setDayStart] = useState("07:00");
  const [nightStart, setNightStart] = useState("19:30");
  const { push, node } = useToast();

  // 从后端加载昼夜配置
  useEffect(() => {
    api
      .getFullConfig()
      .then((cfg) => {
        setEnabled(cfg.display.day_night.enabled);
        setTransition(cfg.display.day_night.transition_minutes);
        setDayStart(cfg.display.day_night.day_start);
        setNightStart(cfg.display.day_night.night_start);
      })
      .catch(() => {});
  }, []);

  // 防抖写入（700ms）
  const dnTimer = useRef<number | null>(null);
  const scheduleWrite = (e: boolean, t: number, ds: string, ns: string) => {
    if (dnTimer.current !== null) window.clearTimeout(dnTimer.current);
    dnTimer.current = window.setTimeout(async () => {
      try {
        await api.setDayNightConfig({
          enabled: e,
          transition_minutes: Math.max(1, t),
          mode: "custom_times",
          day_start: ds || "07:00",
          night_start: ns || "19:30",
        });
        push("昼夜设置已保存");
      } catch (err) {
        push(`保存失败：${(err as Error).message}`, "err");
      }
    }, 700);
  };

  return (
    <div className="space-y-4">
      <Section title="昼夜节律" desc="白天使用日间色温，夜间自动过渡到暖色，匹配生理节律。">
        <div className="flex items-center justify-between">
          <span className="text-xs text-zinc-300">启用昼夜自动切换</span>
          <Switch
            checked={enabled}
            onChange={(v) => {
              setEnabled(v);
              scheduleWrite(v, transition, dayStart, nightStart);
            }}
          />
        </div>
      </Section>

      <Section title="时间与过渡">
        <div className="grid grid-cols-3 gap-4">
          <Field label="白天开始">
            <input
              type="time"
              className="input w-full"
              value={dayStart}
              onChange={(e) => {
                setDayStart(e.target.value);
                scheduleWrite(enabled, transition, e.target.value, nightStart);
              }}
            />
          </Field>
          <Field label="夜间开始">
            <input
              type="time"
              className="input w-full"
              value={nightStart}
              onChange={(e) => {
                setNightStart(e.target.value);
                scheduleWrite(enabled, transition, dayStart, e.target.value);
              }}
            />
          </Field>
          <Field label="过渡时长（分钟）" hint="色温渐变，避免突变">
            <input
              type="number"
              min={1}
              max={180}
              className="input w-full"
              value={transition}
              onChange={(e) => {
                const t = Math.max(1, Number(e.target.value));
                setTransition(t);
                scheduleWrite(enabled, t, dayStart, nightStart);
              }}
            />
          </Field>
        </div>
      </Section>

      <div className="rounded-xl border border-surface-border bg-surface-raised p-4">
        <div className="flex items-center justify-between text-xs text-zinc-400">
          <span>☀ 白天 {dayStart || "07:00"}</span>
          <span className="text-zinc-600">过渡 {transition} 分钟</span>
          <span>☾ 夜间 {nightStart || "19:30"}</span>
        </div>
        <div className="mt-3 flex h-2 overflow-hidden rounded-full">
          <div className="bg-amber-400/80" style={{ width: "30%" }} />
          <div className="bg-gradient-to-r from-amber-400/60 to-indigo-400/70" style={{ width: "15%" }} />
          <div className="bg-indigo-400/60" style={{ width: "55%" }} />
        </div>
      </div>

      <Note>
        日落表模式（按地理位置自动推算）将在 v0.2 提供；MVP 使用自定义时间。
      </Note>
      {node}
    </div>
  );
}

// ---------------------------------------------------------------------------
// 6. 快捷键
// ---------------------------------------------------------------------------

function ShortcutsTab() {
  const [shortcuts, setShortcuts] = useState<api.ShortcutConfig>(DEFAULT_DRAFT.shortcuts);
  const [saving, setSaving] = useState(false);
  const { push, node } = useToast();

  useEffect(() => {
    api.getFullConfig().then((cfg) => setShortcuts(cfg.shortcuts)).catch(() => {});
  }, []);

  const save = async () => {
    setSaving(true);
    try {
      await api.setShortcuts(shortcuts);
      push("快捷键已保存并生效");
    } catch (e) {
      push(`快捷键注册失败：${(e as Error).message}`, "err");
    } finally {
      setSaving(false);
    }
  };

  const rows: { key: keyof api.ShortcutConfig; label: string }[] = [
    { key: "toggle_filter", label: "开关滤镜" },
    { key: "toggle_safe_mode", label: "切换滤镜旁路（取色友好）" },
    { key: "start_break", label: "开始引导休息" },
  ];
  return (
    <div className="space-y-4">
      <Section title="全局快捷键" desc="使用 Ctrl+Alt+F 等格式；保存后立即注册，即使设置窗口隐藏也可使用。">
        <div className="space-y-2 text-xs text-zinc-400">
          {rows.map((row) => (
            <label key={row.key} className="flex items-center justify-between gap-4 rounded-lg border border-surface-border bg-surface-raised px-3 py-2.5">
              <span>{row.label}</span>
              <input className="input w-40 font-mono text-xs" value={shortcuts[row.key]} onChange={(e) => setShortcuts((s) => ({ ...s, [row.key]: e.target.value }))} />
            </label>
          ))}
        </div>
        <button type="button" className="btn-primary mt-3" disabled={saving} onClick={save}>{saving ? "注册中…" : "保存并注册"}</button>
      </Section>
      <Note>
        若组合键已被系统或另一应用占用，保存会失败且原有配置保持不变。
      </Note>
      {node}
    </div>
  );
}

// ---------------------------------------------------------------------------
// 7. 通用 / 隐私
// ---------------------------------------------------------------------------

function GeneralTab({ onImported }: { onImported?: () => void }) {
  const [autoStart, setAutoStartState] = useState(false);
  const [busy, setBusy] = useState(false);
  const fileRef = useRef<HTMLInputElement>(null);
  const { push, node } = useToast();

  useEffect(() => {
    api.getAutoStart().then(setAutoStartState).catch(() => {});
  }, []);

  const setAutoStart = async (on: boolean) => {
    setAutoStartState(on);
    try {
      await api.setAutoStart(on);
      push(on ? "已开启开机自启" : "已关闭开机自启");
    } catch (e) {
      setAutoStartState(!on);
      push(`自启设置失败：${(e as Error).message}`, "err");
    }
  };

  const doExport = async () => {
    try {
      const bundle: ExportBundle = await api.exportConfig();
      const blob = new Blob([JSON.stringify(bundle, null, 2)], {
        type: "application/json",
      });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      const date = new Date().toISOString().slice(0, 10);
      a.href = url;
      a.download = `eyescare-config-${date}.json`;
      a.click();
      // Firefox 需要延迟 revoke，否则下载可能中断
      window.setTimeout(() => URL.revokeObjectURL(url), 1000);
      push("配置已导出（含规则，可完整迁移）");
    } catch (e) {
      push(`导出失败：${(e as Error).message}`, "err");
    }
  };

  const doImport = async (file: File) => {
    setBusy(true);
    try {
      const text = await file.text();
      const bundle = JSON.parse(text) as ExportBundle;
      if (!bundle.config || !bundle.rules) {
        throw new Error("文件格式不正确：缺少 config 或 rules");
      }
      const outcome = await api.importConfig(bundle);
      push(`导入成功：${outcome.rules_imported} 条规则已合并`);
      onImported?.();
    } catch (e) {
      push(`导入失败：${(e as Error).message}`, "err");
    } finally {
      setBusy(false);
      if (fileRef.current) fileRef.current.value = "";
    }
  };

  return (
    <div className="space-y-4">
      <Section title="通用">
        <div className="flex items-center justify-between">
          <div>
            <div className="text-xs text-zinc-300">开机自启</div>
            <div className="text-[11px] text-zinc-600">登录 Windows 后自动在托盘运行</div>
          </div>
          <Switch checked={autoStart} onChange={setAutoStart} />
        </div>
      </Section>

      <Section title="隐私" desc="本机运行，不采集、不上报。">
        <ul className="list-disc space-y-1.5 pl-4 text-xs leading-relaxed text-zinc-400">
          <li>无遥测、无网络上报，配置与洞察只留在本机。</li>
          <li>前台应用身份默认哈希后落库，明文显示名默认关闭。</li>
          <li>不请求摄像头、屏幕录制、日历等无关权限。</li>
        </ul>
      </Section>

      <Section title="配置迁移" desc="导出为 JSON 文件，可在另一台设备导入，完整迁移设置与规则。">
        <div className="flex gap-3">
          <button type="button" className="btn-primary" onClick={doExport}>
            ⭳ 导出配置
          </button>
          <button type="button" className="btn-ghost" onClick={() => fileRef.current?.click()} disabled={busy}>
            {busy ? "导入中…" : "⭱ 导入配置"}
          </button>
          <input
            ref={fileRef}
            type="file"
            accept="application/json,.json"
            className="hidden"
            onChange={(e) => {
              const f = e.target.files?.[0];
              if (f) doImport(f);
            }}
          />
        </div>
      </Section>
      {node}
    </div>
  );
}

// ---------------------------------------------------------------------------
// 8. 关于
// ---------------------------------------------------------------------------

function AboutTab({ version }: { version: string }) {
  return (
    <div className="space-y-4">
      <div className="card flex flex-col items-center py-10 text-center">
        <BrandMark className="h-16 w-16" />
        <h2 className="mt-4 text-xl font-semibold tracking-wide">EyesCare</h2>
        <p className="mt-1 text-xs" style={{ color: "var(--muted)" }}>
          荷鲁斯之眼 · 隐私优先的护眼节律助手
        </p>
        <div className="mt-3 rounded-full border border-surface-border bg-surface-raised px-3 py-1 text-xs text-zinc-400">
          版本 {version || "…"}
        </div>
      </div>

      <Section title="隐私承诺">
        <ul className="list-disc space-y-1.5 pl-4 text-xs leading-relaxed text-zinc-400">
          <li>默认无网络、无遥测，所有数据留在本机。</li>
          <li>前台应用身份默认哈希化后落库，明文显示名可配置且默认关闭。</li>
          <li>洞察数据 90 天保留，到期自动清理。</li>
          <li>不请求摄像头、屏幕录制、日历等无关权限。</li>
        </ul>
      </Section>

      <Section title="技术">
        <div className="grid grid-cols-2 gap-2 text-xs text-zinc-400">
          <div className="rounded-lg bg-surface-raised px-3 py-2">界面 · React 18 + Tailwind</div>
          <div className="rounded-lg bg-surface-raised px-3 py-2">外壳 · Tauri 2（Rust）</div>
          <div className="rounded-lg bg-surface-raised px-3 py-2">洞察存储 · SQLite（本地）</div>
          <div className="rounded-lg bg-surface-raised px-3 py-2">滤镜 · GDI Gamma（截图友好）</div>
        </div>
      </Section>
    </div>
  );
}
