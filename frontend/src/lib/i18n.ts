// i18n — 6 languages, auto-detected from the browser/OS locale (navigator.language), EN fallback, manual switch.
import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import en from "../locales/en.json";
import ru from "../locales/ru.json";
import zh from "../locales/zh.json";
import es from "../locales/es.json";
import pt from "../locales/pt.json";
import fr from "../locales/fr.json";

export const LANGS = ["en", "ru", "zh", "es", "pt", "fr"] as const;
export type Lang = (typeof LANGS)[number];

// Языки КОНТЕНТА (источник/цель дубляжа) — шире локалей интерфейса. Весь стек их тянет:
// Higgs TTS v3 (100+ языков озвучки/клона), Gemma-4 перевод (100+), Whisper large-v3 ASR (99),
// Parakeet v3 — 25 европейских. Субтитры любой письменности (CJK/арабица/деванагари) рендерятся
// через системный fallback шрифтов Windows. Нативные имена — как язык называет сам себя.
export const DUB_LANGS: { code: string; name: string }[] = [
  { code: "en", name: "English" },      { code: "zh", name: "中文" },
  { code: "es", name: "Español" },      { code: "hi", name: "हिन्दी" },
  { code: "ar", name: "العربية" },      { code: "pt", name: "Português" },
  { code: "ru", name: "Русский" },      { code: "fr", name: "Français" },
  { code: "de", name: "Deutsch" },      { code: "ja", name: "日本語" },
  { code: "ko", name: "한국어" },        { code: "it", name: "Italiano" },
  { code: "tr", name: "Türkçe" },       { code: "pl", name: "Polski" },
  { code: "nl", name: "Nederlands" },   { code: "uk", name: "Українська" },
  { code: "id", name: "Bahasa Indonesia" }, { code: "vi", name: "Tiếng Việt" },
  { code: "th", name: "ไทย" },          { code: "fa", name: "فارسی" },
  { code: "he", name: "עברית" },        { code: "cs", name: "Čeština" },
  { code: "sv", name: "Svenska" },      { code: "ro", name: "Română" },
  { code: "el", name: "Ελληνικά" },     { code: "hu", name: "Magyar" },
  { code: "da", name: "Dansk" },        { code: "fi", name: "Suomi" },
  { code: "bg", name: "Български" },     { code: "hr", name: "Hrvatski" },
];

const detect = (): Lang => {
  const stored = localStorage.getItem("lang");
  if (stored && (LANGS as readonly string[]).includes(stored)) return stored as Lang;
  const nav = (navigator.language || "en").slice(0, 2).toLowerCase();
  return (LANGS as readonly string[]).includes(nav) ? (nav as Lang) : "en";
};

i18n.use(initReactI18next).init({
  resources: { en: { t: en }, ru: { t: ru }, zh: { t: zh }, es: { t: es }, pt: { t: pt }, fr: { t: fr } },
  lng: detect(),
  fallbackLng: "en",
  defaultNS: "t",
  interpolation: { escapeValue: false },
});

// Keep <html lang> in sync with the ACTIVE language. The static index.html ships lang="en", which made
// Chrome treat the page as English (offering to "translate from English") even when the UI is Russian.
const _applyHtmlLang = (l: string) => { try { document.documentElement.lang = l; } catch { /* SSR/no-DOM */ } };
_applyHtmlLang(i18n.language);
i18n.on("languageChanged", _applyHtmlLang);

export const setLang = (l: Lang) => { localStorage.setItem("lang", l); i18n.changeLanguage(l); };
export default i18n;
