import { createTranslator } from '@onlyne/node-host/i18n';

const en = {
  'cli.usage': 'usage: mcp-gateway <config.json>',
  'startup.pending': 'The gateway is starting.',
  'config.unreadable': 'The configuration file is unreadable: {path}',
  'config.object': 'The configuration needs {field} as an object.',
  'config.string': 'The configuration field {field} is a non-empty string.',
  'config.servers': 'The configuration needs servers as an array.',
  'config.duplicateName': 'Two servers share the name {name}.',
  'config.language': 'The configuration language is en or zh-CN, not {value}.',
  'error.claimIdentity': 'HTTP callers cannot claim the gateway identity.',
  'error.decide': 'The gateway forwards tool calls; it decides nothing.',
  'error.noContext': 'The gateway consumes no model context.',
} as const;

const zh: Record<keyof typeof en, string> = {
  'cli.usage': '用法：mcp-gateway <config.json>',
  'startup.pending': '网关正在启动。',
  'config.unreadable': '配置文件无法读取：{path}',
  'config.object': '配置项 {field} 需要是一个对象。',
  'config.string': '配置项 {field} 需要是非空字符串。',
  'config.servers': '配置项 servers 需要是一个数组。',
  'config.duplicateName': '有两个服务器共用了名称 {name}。',
  'config.language': '配置项 language 只能是 en 或 zh-CN，而不是 {value}。',
  'error.claimIdentity': 'HTTP 调用方不能冒用网关身份。',
  'error.decide': '网关只转发工具调用，不做任何决策。',
  'error.noContext': '网关不消费模型上下文。',
};

export type GatewayTextKey = keyof typeof en;

export const t = createTranslator({ en, 'zh-CN': zh });
