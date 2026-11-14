import { formatVars, getLocale, pickText } from "../i18n.ts";

const MESSAGES = {
	schemeRequired: [
		"Onlyne URL must use the 'onlyne://' scheme: {url}",
		"Onlyne URL 必须使用 'onlyne://' 协议：{url}",
	],
	malformedEncoding: [
		"Malformed URL encoding in '{path}'.",
		"URL 编码无效：'{path}'。",
	],
	routeShape: [
		"Invalid route shape; expected: {shape}",
		"路径格式无效，期望：{shape}",
	],
	unknownRoute: [
		"Unknown onlyne:// route host '{host}'. Valid hosts: neighbors, threads, inbox, outbound, conversations, messages, contexts, resources, todo.",
		"未知 onlyne:// 路由 '{host}'。可用路由：neighbors、threads、inbox、outbound、conversations、messages、contexts、resources、todo。",
	],
	privateSession: [
		"Route '{view}' is restricted to active sessions.",
		"路由 '{view}' 仅限活跃会话读取。",
	],
	mainSessionOnly: [
		"Route '{view}' is restricted to the main Antigravity session.",
		"路由 '{view}' 仅限主 Antigravity 会话读取。",
	],
	rootTitle: [
		"# Onlyne Node `{node}`",
		"# Onlyne 节点 `{node}`",
	],
	rootRegion: [
		"- Region: `{region}`",
		"- 区域（Region）：`{region}`",
	],
	rootEndpoint: [
		"- Endpoint: `{endpoint}`",
		"- 地址（Endpoint）：`{endpoint}`",
	],
	rootDomain: [
		"- Active domains: {domains}",
		"- 活跃域：{domains}",
	],
	rootDomainNone: [
		"none",
		"无",
	],
	rootDomainUnavailable: [
		"- Domains unavailable: {message}",
		"- 域列表暂不可用：{message}",
	],
	rootViews: [
		"Available views: `onlyne://neighbors`, `onlyne://threads`, `onlyne://inbox`, `onlyne://outbound`, `onlyne://conversations`, `onlyne://contexts`, `onlyne://resources`, `onlyne://todo`.",
		"可用视图：`onlyne://neighbors`、`onlyne://threads`、`onlyne://inbox`、`onlyne://outbound`、`onlyne://conversations`、`onlyne://contexts`、`onlyne://resources`、`onlyne://todo`。",
	],
} as const;

export type UrlsTextKey = keyof typeof MESSAGES;

export function urlsText(key: UrlsTextKey, vars?: Record<string, string | number>): string {
	const [en, zh] = MESSAGES[key];
	return formatVars(pickText(en, zh, getLocale()), vars);
}
