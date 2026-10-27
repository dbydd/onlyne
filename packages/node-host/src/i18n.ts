export type Locale = 'en' | 'zh-CN';
export const LOCALES: readonly Locale[] = ['en', 'zh-CN'];

export function parseLocale(value: string | null | undefined): Locale | undefined {
  const language = value?.trim().toLowerCase().replace(/_/g, '-');
  if (language === 'en' || language?.startsWith('en-') || language?.startsWith('en.')) return 'en';
  if (language === 'zh' || language?.startsWith('zh-') || language?.startsWith('zh.')) return 'zh-CN';
  return undefined;
}

export function resolveLocale(explicit?: string | null, env: Record<string, string | undefined> = process.env): Locale {
  return parseLocale(explicit) ?? parseLocale(env.ONLYNE_LANG) ?? parseLocale(env.LC_ALL || env.LANG) ?? 'en';
}

let locale: Locale = resolveLocale();
export function getLocale(): Locale { return locale; }
export function setLocale(value: Locale): void { locale = value; }
export type Catalog<K extends string> = Record<Locale, Record<K, string>>;

export function createTranslator<K extends string>(catalog: Catalog<K>): (key: K, vars?: Record<string, string | number>) => string {
  return (key, vars) => catalog[getLocale()][key].replace(/\{(\w+)\}/g, (match, name: string) => vars?.[name] === undefined ? match : String(vars[name]));
}
