/**
 * UI language: English, Simplified Chinese and Traditional Chinese.
 *
 * The person picks a language in Settings › General, or `auto` follows the
 * system's preferred languages: the first Chinese or English tag wins, and
 * anything else (or nothing readable) falls back to English. Script and region
 * decide the Chinese variant: Hant, TW, HK and MO are Traditional; Hans, CN, SG
 * and MY are Simplified (Singapore officially writes Simplified Chinese).
 */

import { createContext, useContext, useMemo, type ReactNode } from "react";
import { MESSAGES, type MessageKey } from "./messages";

export type Language = "en" | "zh-Hans" | "zh-Hant";
export type LanguagePreference = "auto" | Language;

export const LANGUAGE_OPTIONS: LanguagePreference[] = ["auto", "en", "zh-Hans", "zh-Hant"];

/** The name of each language in that language (the conventional menu form). */
export const LANGUAGE_NAMES: Record<Language, string> = {
  en: "English",
  "zh-Hans": "简体中文",
  "zh-Hant": "繁體中文",
};

/** Map one BCP 47 tag to a supported language, or `null`. */
export function languageOfTag(tag: string): Language | null {
  const lower = tag.trim().toLowerCase().replace(/_/g, "-");
  if (lower === "en" || lower.startsWith("en-")) return "en";
  if (lower !== "zh" && !lower.startsWith("zh-")) return null;
  const parts = lower.split("-").slice(1);
  if (parts.includes("hant")) return "zh-Hant";
  if (parts.includes("hans")) return "zh-Hans";
  if (parts.some((part) => part === "tw" || part === "hk" || part === "mo")) return "zh-Hant";
  return "zh-Hans";
}

/** The first supported language among the system's preferences, else English. */
export function detectLanguage(languages: readonly string[] | undefined): Language {
  for (const tag of languages ?? []) {
    const language = languageOfTag(tag);
    if (language) return language;
  }
  return "en";
}

export function systemLanguages(): readonly string[] {
  if (typeof navigator === "undefined") return [];
  return navigator.languages?.length ? navigator.languages : navigator.language ? [navigator.language] : [];
}

export function resolveLanguage(
  preference: string | undefined,
  languages: readonly string[] = systemLanguages(),
): Language {
  if (preference === "en" || preference === "zh-Hans" || preference === "zh-Hant") return preference;
  return detectLanguage(languages);
}

export type Translate = (key: MessageKey, vars?: Record<string, string | number>) => string;

export function translator(language: Language): Translate {
  const table = MESSAGES[language];
  return (key, vars) => {
    const text = table[key] ?? MESSAGES.en[key] ?? key;
    if (!vars) return text;
    return text.replace(/\{(\w+)\}/g, (match: string, name: string) =>
      name in vars ? String(vars[name]) : match,
    );
  };
}

/** The language the UI shows now, for plain helpers outside React. */
let activeLanguage: Language = "en";

/** The language the UI shows now. */
export function currentLanguage(): Language {
  return activeLanguage;
}

/** Translate in the active language (helpers such as time formatting). */
export function tx(key: MessageKey, vars?: Record<string, string | number>): string {
  return translator(activeLanguage)(key, vars);
}

interface I18n {
  language: Language;
  t: Translate;
}

const I18nContext = createContext<I18n>({ language: "en", t: translator("en") });

export function I18nProvider({ preference, children }: { preference: string | undefined; children: ReactNode }) {
  const language = resolveLanguage(preference);
  activeLanguage = language;
  const value = useMemo(() => ({ language, t: translator(language) }), [language]);
  if (typeof document !== "undefined" && document.documentElement.lang !== language) {
    document.documentElement.lang = language;
  }
  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

export function useI18n(): I18n {
  return useContext(I18nContext);
}

export function useT(): Translate {
  return useContext(I18nContext).t;
}

export type { MessageKey };
