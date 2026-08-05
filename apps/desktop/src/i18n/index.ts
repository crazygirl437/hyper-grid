import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import de from "./de.json";
import en from "./en.json";
import es from "./es.json";
import fr from "./fr.json";
import ja from "./ja.json";
import ko from "./ko.json";
import ptBR from "./pt-BR.json";
import ru from "./ru.json";
import zh from "./zh-CN.json";

/** Supported UI locales (native labels for the language picker). */
export const SUPPORTED_LOCALES = [
  { code: "zh-CN", label: "中文" },
  { code: "en", label: "English" },
  { code: "ja", label: "日本語" },
  { code: "ko", label: "한국어" },
  { code: "es", label: "Español" },
  { code: "fr", label: "Français" },
  { code: "de", label: "Deutsch" },
  { code: "pt-BR", label: "Português" },
  { code: "ru", label: "Русский" },
] as const;

export type LocaleCode = (typeof SUPPORTED_LOCALES)[number]["code"];

const SUPPORTED_CODES = new Set<string>(SUPPORTED_LOCALES.map((l) => l.code));

/** Map navigator / stored language to a supported locale code. */
export function resolveLocale(raw?: string | null): LocaleCode {
  if (!raw) return detectSystemLocale();
  const lower = raw.toLowerCase();
  if (SUPPORTED_CODES.has(raw)) return raw as LocaleCode;
  if (lower.startsWith("zh")) return "zh-CN";
  if (lower.startsWith("pt")) return "pt-BR";
  if (lower.startsWith("ja")) return "ja";
  if (lower.startsWith("ko")) return "ko";
  if (lower.startsWith("es")) return "es";
  if (lower.startsWith("fr")) return "fr";
  if (lower.startsWith("de")) return "de";
  if (lower.startsWith("ru")) return "ru";
  if (lower.startsWith("en")) return "en";
  return "en";
}

function detectSystemLocale(): LocaleCode {
  if (typeof navigator === "undefined") return "en";
  return resolveLocale(navigator.language || (navigator as Navigator & { userLanguage?: string }).userLanguage);
}

void i18n.use(initReactI18next).init({
  resources: {
    en: { translation: en },
    "zh-CN": { translation: zh },
    ja: { translation: ja },
    ko: { translation: ko },
    es: { translation: es },
    fr: { translation: fr },
    de: { translation: de },
    "pt-BR": { translation: ptBR },
    ru: { translation: ru },
  },
  lng: detectSystemLocale(),
  fallbackLng: "en",
  interpolation: { escapeValue: false },
});

export default i18n;
