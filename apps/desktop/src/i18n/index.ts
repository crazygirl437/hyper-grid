import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import en from "./en.json";
import zh from "./zh-CN.json";

const systemLang =
  typeof navigator !== "undefined" && navigator.language.toLowerCase().startsWith("zh")
    ? "zh-CN"
    : "en";

void i18n.use(initReactI18next).init({
  resources: {
    en: { translation: en },
    "zh-CN": { translation: zh },
  },
  lng: systemLang,
  fallbackLng: "en",
  interpolation: { escapeValue: false },
});

export default i18n;
