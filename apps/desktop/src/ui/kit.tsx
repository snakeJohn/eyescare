import { useCallback, useState, type ReactNode } from "react";

export function Switch({
  checked,
  onChange,
  disabled,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={`relative h-5 w-9 shrink-0 rounded-full transition-colors disabled:opacity-40 ${
        checked ? "bg-brand-500" : "bg-[var(--track)]"
      }`}
    >
      <span
        className={`absolute left-0.5 top-0.5 h-4 w-4 rounded-full bg-white shadow transition-transform ${
          checked ? "translate-x-4" : ""
        }`}
      />
    </button>
  );
}

export function Section({
  title,
  desc,
  children,
  className = "",
}: {
  title: string;
  desc?: string;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section className={`card ${className}`}>
      <h3 className="text-sm font-semibold text-[var(--fg)]">{title}</h3>
      {desc && <p className="mt-0.5 text-xs leading-relaxed text-[var(--muted)]">{desc}</p>}
      <div className="mt-3">{children}</div>
    </section>
  );
}

export function Field({
  label,
  children,
  hint,
}: {
  label: string;
  children: ReactNode;
  hint?: string;
}) {
  return (
    <label className="block">
      <span className="label">{label}</span>
      <div className="mt-1.5">{children}</div>
      {hint && <span className="mt-1 block text-[11px] text-[var(--faint)]">{hint}</span>}
    </label>
  );
}

export function StatCard({
  label,
  value,
  sub,
  accent,
}: {
  label: string;
  value: string;
  sub?: string;
  accent?: boolean;
}) {
  return (
    <div
      className={`rounded-xl border p-3.5 ${
        accent ? "border-brand-500/40 bg-brand-500/10" : "border-[var(--line)] bg-[var(--raised)]"
      }`}
    >
      <div className="text-[11px] text-[var(--muted)]">{label}</div>
      <div className={`mt-1 text-xl font-semibold ${accent ? "text-brand-300" : "text-[var(--fg)]"}`}>
        {value}
      </div>
      {sub && <div className="mt-0.5 text-[11px] text-[var(--faint)]">{sub}</div>}
    </div>
  );
}

export function Note({ children }: { children: ReactNode }) {
  return (
    <p className="mt-3 rounded-lg border border-[var(--line)] bg-[var(--raised)]/70 px-3 py-2 text-[11px] leading-relaxed text-[var(--muted)]">
      {children}
    </p>
  );
}

export function useToast() {
  const [toasts, setToasts] = useState<{ id: number; kind: "ok" | "err"; text: string }[]>([]);
  const push = useCallback((text: string, kind: "ok" | "err" = "ok") => {
    const id = Date.now() + Math.random();
    setToasts((t) => [...t, { id, kind, text }]);
    window.setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), 4000);
  }, []);
  const node = (
    <div className="pointer-events-none fixed bottom-4 right-4 z-50 flex flex-col gap-2">
      {toasts.map((t) => (
        <div
          key={t.id}
          className={`pointer-events-auto rounded-lg border px-3.5 py-2 text-xs shadow-lg ${
            t.kind === "ok"
              ? "border-brand-500/40 bg-[var(--card)] text-brand-300"
              : "border-red-800 bg-[var(--card)] text-red-300"
          }`}
        >
          {t.text}
        </div>
      ))}
    </div>
  );
  return { push, node };
}
