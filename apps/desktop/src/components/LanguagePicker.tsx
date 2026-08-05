import { useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  resolveLocale,
  SUPPORTED_LOCALES,
  type LocaleCode,
} from "../i18n";

type Props = {
  value: string;
  onChange: (code: string) => void;
};

function Star({
  cx,
  cy,
  r,
  fill = "#FFDE00",
}: {
  cx: number;
  cy: number;
  r: number;
  fill?: string;
}) {
  const pts: string[] = [];
  for (let i = 0; i < 5; i++) {
    const a = (-Math.PI / 2) + (i * 2 * Math.PI) / 5;
    const b = a + Math.PI / 5;
    pts.push(`${cx + Math.cos(a) * r},${cy + Math.sin(a) * r}`);
    pts.push(`${cx + Math.cos(b) * r * 0.38},${cy + Math.sin(b) * r * 0.38}`);
  }
  return <polygon fill={fill} points={pts.join(" ")} />;
}

/** Compact SVG flags — render reliably on Linux where emoji flags often fail. */
function FlagIcon({ code, className }: { code: LocaleCode; className?: string }) {
  const common = {
    className,
    viewBox: "0 0 24 16",
    width: 22,
    height: 15,
    "aria-hidden": true as const,
  };
  switch (code) {
    case "zh-CN":
      return (
        <svg {...common}>
          <rect width="24" height="16" fill="#DE2910" rx="1.5" />
          <Star cx={5} cy={4.2} r={2.1} />
          <g transform="translate(9.6 1.6)">
            <Star cx={1.6} cy={1.2} r={0.7} />
            <Star cx={3.2} cy={2.4} r={0.7} />
            <Star cx={3.5} cy={4.2} r={0.7} />
            <Star cx={2.4} cy={5.7} r={0.7} />
          </g>
        </svg>
      );
    case "en":
      return (
        <svg {...common}>
          <rect width="24" height="16" fill="#B22234" rx="1.5" />
          <path
            fill="#fff"
            d="M0 1.85h24v1.45H0zm0 2.9h24v1.45H0zm0 2.9h24v1.45H0zm0 2.9h24v1.45H0zm0 2.9h24v1.45H0z"
          />
          <rect width="10.2" height="8.6" fill="#3C3B6E" />
          <g fill="#fff">
            {[1.6, 3.4, 5.2, 7, 8.8].flatMap((x, xi) =>
              [1.5, 3.1, 4.7, 6.3, 7.9]
                .filter((_, yi) => (xi + yi) % 2 === 0)
                .map((y) => (
                  <circle key={`${x}-${y}`} cx={x} cy={y} r={0.32} />
                )),
            )}
          </g>
        </svg>
      );
    case "ja":
      return (
        <svg {...common}>
          <rect width="24" height="16" fill="#fff" stroke="#e5e7eb" strokeWidth="0.6" rx="1.5" />
          <circle cx="12" cy="8" r="4.2" fill="#BC002D" />
        </svg>
      );
    case "ko":
      return (
        <svg {...common}>
          <rect width="24" height="16" fill="#fff" stroke="#e5e7eb" strokeWidth="0.6" rx="1.5" />
          <circle cx="12" cy="8" r="3.35" fill="#CD2E3A" />
          <path d="M8.65 8a3.35 3.35 0 0 0 6.7 0" fill="#0047A0" />
          <g stroke="#000" strokeWidth="0.85" strokeLinecap="square">
            <path d="M5.4 3.6l2.4-1.4M5.95 4.55l2.4-1.4M6.5 5.5l2.4-1.4" />
            <path d="M16.2 12.4l2.4-1.4M16.75 13.35l2.4-1.4M17.3 14.3l2.4-1.4" />
            <path d="M16.2 3.6l2.4 1.4M16.75 4.55l2.4 1.4M18.1 4.3l1.1.65M15.85 5.85l1.1.65" />
            <path d="M5.4 12.4l2.4 1.4M5.95 13.35l2.4 1.4M7.3 13.1l1.1.65M5.05 14.65l1.1.65" />
          </g>
        </svg>
      );
    case "es":
      return (
        <svg {...common}>
          <rect width="24" height="16" fill="#AA151B" rx="1.5" />
          <rect y="4" width="24" height="8" fill="#F1BF00" />
        </svg>
      );
    case "fr":
      return (
        <svg {...common}>
          <rect width="24" height="16" fill="#fff" rx="1.5" />
          <rect width="8" height="16" fill="#002395" />
          <rect x="16" width="8" height="16" fill="#ED2939" />
        </svg>
      );
    case "de":
      return (
        <svg {...common}>
          <rect width="24" height="16" fill="#000" rx="1.5" />
          <rect y="5.33" width="24" height="5.34" fill="#D00" />
          <rect y="10.67" width="24" height="5.33" fill="#FFCE00" />
        </svg>
      );
    case "pt-BR":
      return (
        <svg {...common}>
          <rect width="24" height="16" fill="#009C3B" rx="1.5" />
          <path fill="#FFDF00" d="M12 2.1 21.4 8 12 13.9 2.6 8Z" />
          <circle cx="12" cy="8" r="3.05" fill="#002776" />
          <path
            fill="none"
            stroke="#fff"
            strokeWidth="0.65"
            d="M9.15 8.7c1.35-1.05 3.35-1.35 5.35-.75"
          />
        </svg>
      );
    case "ru":
      return (
        <svg {...common}>
          <rect width="24" height="16" fill="#fff" rx="1.5" />
          <rect y="5.33" width="24" height="5.34" fill="#0039A6" />
          <rect y="10.67" width="24" height="5.33" fill="#D52B1E" />
        </svg>
      );
    default:
      return null;
  }
}

export function LanguagePicker({ value, onChange }: Props) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const listId = useId();
  const current = resolveLocale(value);
  const currentMeta =
    SUPPORTED_LOCALES.find((l) => l.code === current) ?? SUPPORTED_LOCALES[1];

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <div className={`lang-picker${open ? " open" : ""}`} ref={rootRef}>
      <button
        type="button"
        className="lang-picker-trigger"
        aria-label={t("app.language")}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={listId}
        onClick={() => setOpen((v) => !v)}
      >
        <span className="lang-flag-wrap">
          <FlagIcon code={currentMeta.code} className="lang-flag" />
        </span>
        <span className="lang-picker-label">{currentMeta.label}</span>
        <span className="lang-picker-chevron" aria-hidden="true" />
      </button>
      {open && (
        <ul
          id={listId}
          className="lang-picker-menu"
          role="listbox"
          aria-label={t("app.language")}
        >
          {SUPPORTED_LOCALES.map((lng) => {
            const active = lng.code === current;
            return (
              <li key={lng.code} role="presentation">
                <button
                  type="button"
                  role="option"
                  aria-selected={active}
                  className={`lang-picker-option${active ? " active" : ""}`}
                  onClick={() => {
                    onChange(lng.code);
                    setOpen(false);
                  }}
                >
                  <span className="lang-flag-wrap">
                    <FlagIcon code={lng.code} className="lang-flag" />
                  </span>
                  <span className="lang-picker-option-label">{lng.label}</span>
                  {active ? (
                    <span className="lang-picker-check" aria-hidden="true">
                      ✓
                    </span>
                  ) : (
                    <span className="lang-picker-check spacer" aria-hidden="true" />
                  )}
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
