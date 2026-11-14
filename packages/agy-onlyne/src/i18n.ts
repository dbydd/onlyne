import { getLocale as hostGetLocale, setLocale as hostSetLocale, type Locale } from "@onlyne/node-host/i18n";

export type { Locale };

export function getLocale(): Locale {
	return hostGetLocale();
}

export function setLocale(next: Locale): Locale {
	hostSetLocale(next);
	return hostGetLocale();
}

export function resolveLocale(language?: string): Locale {
	if (language === "zh-CN" || language === "en") return language;
	return getLocale();
}

export function pickText(en: string, zhCn: string, locale = getLocale()): string {
	return locale === "zh-CN" ? zhCn : en;
}

export function formatVars(template: string, vars?: Record<string, string | number>): string {
	if (vars === undefined) return template;
	return template.replace(/\{([a-zA-Z0-9_]+)\}/g, (_match, key: string) => {
		const value = vars[key];
		return value === undefined ? `{${key}}` : String(value);
	});
}
