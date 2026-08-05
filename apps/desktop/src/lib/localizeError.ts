import type { TFunction } from "i18next";

/**
 * Backend returns machine-readable errors as:
 *   i18n:<code>
 *   i18n:<code>|key=value|key=value
 * Unknown / legacy strings are returned unchanged (or via errors.unknown when prefixed).
 */
export function localizeError(raw: unknown, t: TFunction): string {
  const msg = String(raw ?? "");
  if (!msg.startsWith("i18n:")) return msg;

  const body = msg.slice("i18n:".length);
  const parts = body.split("|");
  const code = parts[0]?.trim();
  if (!code) return msg;

  const params: Record<string, string> = {};
  for (let i = 1; i < parts.length; i++) {
    const p = parts[i];
    const eq = p.indexOf("=");
    if (eq <= 0) continue;
    params[p.slice(0, eq)] = p.slice(eq + 1);
  }

  const key = `errors.${code}`;
  const out = t(key, params);
  // i18next returns the key when missing
  if (out === key) {
    return params.detail ? t("errors.unknown", { detail: params.detail }) : msg;
  }
  return out;
}
