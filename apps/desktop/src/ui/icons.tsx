/** Inline marks. Eye-of-Horus brand + nav glyphs. */

export function BrandMark({ className = "h-8 w-8" }: { className?: string }) {
  return (
    <svg viewBox="0 0 32 32" className={className} aria-hidden>
      <rect x="1.5" y="1.5" width="29" height="29" rx="7" fill="#141210" stroke="#a67418" strokeWidth="1" />
      <path
        d="M6.4 12.2c3.2-2.4 7-3.4 10.1-3.2 3 .2 5.7 1.4 7.6 2.8"
        fill="none"
        stroke="#e4b04a"
        strokeWidth="1.7"
        strokeLinecap="round"
      />
      <path
        d="M7.2 15.4c2.8 3.6 6.4 5.2 9.4 5 3.1-.2 5.8-2 7.4-4.4-1.8-2.4-4.6-3.8-7.6-3.7-3 .1-6.3 1.4-9.2 3.1z"
        fill="#fff6e4"
        stroke="#c9922a"
        strokeWidth="1.1"
      />
      <circle cx="15.4" cy="16.1" r="3.1" fill="#c48418" />
      <circle cx="15.4" cy="16.1" r="1.45" fill="#1c140c" />
      <circle cx="16.5" cy="15.2" r=".7" fill="#fff8ee" />
      <path
        d="M16.2 20.6v4.2c0 1.3.7 2.1 1.8 2.4 1.2.3 2.3-.4 2.6-1.4.3-1-.4-1.8-1.3-2.1-.7-.2-1.4.1-1.6.8"
        fill="none"
        stroke="#e4b04a"
        strokeWidth="1.45"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

type IconProps = { className?: string };

function wrap(d: string) {
  return function Glyph({ className = "h-4 w-4" }: IconProps) {
    return (
      <svg viewBox="0 0 24 24" className={className} fill="none" aria-hidden>
        <path d={d} stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" />
      </svg>
    );
  };
}

export const IconDisplay = wrap(
  "M4 12c2.8-5 6.2-7.5 8-7.5S17.2 7 20 12c-2.8 5-6.2 7.5-8 7.5S6.8 17 4 12z M12 9.4a2.6 2.6 0 1 1 0 5.2 2.6 2.6 0 0 1 0-5.2z",
);
export const IconRules = wrap("M5 6.5h14M5 12h9M5 17.5h14M17.5 10.5l2 2 3.2-3.6");
export const IconBreaks = wrap("M12 7v5.2l3 1.8 M12 4.5a7.5 7.5 0 1 1 0 15 7.5 7.5 0 0 1 0-15z");
export const IconInsights = wrap("M5 16.5 9.2 11l3.1 3.2L19 7.5 M5 19.5h14");
export const IconRhythm = wrap("M12 4.5v3.2M12 16.3v3.2M5.8 8.2l2.2 2.2M16 13.6l2.2 2.2M4.5 12h3.2M16.3 12h3.2M5.8 15.8l2.2-2.2M16 10.4l2.2-2.2");
export const IconShortcuts = wrap("M5 8h14v9.5H5z M8 8V6.2M16 8V6.2M8 12.2h.01M12 12.2h.01M16 12.2h.01M8 15.4h8");
export const IconGeneral = wrap("M12 15.2A3.2 3.2 0 1 0 12 8.8a3.2 3.2 0 0 0 0 6.4z M19.2 13.1l1.6.9-1.6 2.8-1.8-.3a6.7 6.7 0 0 1-1.5.9l-.3 1.8h-3.2l-.3-1.8a6.7 6.7 0 0 1-1.5-.9l-1.8.3L3.2 14l1.6-.9a6.8 6.8 0 0 1 0-1.8L3.2 10l1.6-2.8 1.8.3a6.7 6.7 0 0 1 1.5-.9l.3-1.8h3.2l.3 1.8a6.7 6.7 0 0 1 1.5.9l1.8-.3L20.8 10l-1.6.9a6.8 6.8 0 0 1 0 2.2z");
export const IconAbout = wrap("M12 17v-5 M12 8.2h.01 M12 4.5a7.5 7.5 0 1 1 0 15 7.5 7.5 0 0 1 0-15z");
export const IconSun = wrap("M12 7.2a4.8 4.8 0 1 0 0 9.6 4.8 4.8 0 0 0 0-9.6z M12 3.5v1.8M12 18.7v1.8M4.8 12H3M21 12h-1.8M6.2 6.2l1.3 1.3M16.5 16.5l1.3 1.3M6.2 17.8l1.3-1.3M16.5 7.5l1.3-1.3");
export const IconMoon = wrap("M16.8 14.6A6.4 6.4 0 0 1 9.6 7.2 6.6 6.6 0 1 0 16.8 14.6z");
