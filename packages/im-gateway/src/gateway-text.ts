import { createTranslator } from '@onlyne/node-host/i18n';

const en = {
  'cli.usage': 'usage: onlyne-im-gateway <config.json>',
  'startup.pending': 'The gateway is starting.',
  'config.unreadable': 'The configuration file is unreadable: {path}',
  'config.object': 'The configuration needs {field} as an object.',
  'config.string': 'The configuration needs {field} as a non-empty string.',
  'config.connectors': 'The configuration needs connectors as an array.',
  'config.platformTwice': 'The configuration declares platform {platform} twice.',
  'config.language': 'The configuration language is en or zh-CN, not {value}.',
  'error.noConnectorResource': 'No connector serves resource {resource}.',
  'error.noConnectorPlatform': 'No connector serves platform {platform}.',
  'error.noConnectorPackage': 'No connector package serves platform {platform}.',
  'error.claimIdentity': 'HTTP callers cannot claim the gateway identity.',
  'error.decide': 'The gateway forwards messages; it does not decide sends.',
  'error.noContext': 'The gateway consumes no model context.',
} as const;

const zh: Record<keyof typeof en, string> = {
  'cli.usage': '用法：onlyne-im-gateway <config.json>',
  'startup.pending': '网关正在启动。',
  'config.unreadable': '配置文件无法读取：{path}',
  'config.object': '配置项 {field} 需要是一个对象。',
  'config.string': '配置项 {field} 需要是非空字符串。',
  'config.connectors': '配置项 connectors 需要是一个数组。',
  'config.platformTwice': '配置里重复声明了平台 {platform}。',
  'config.language': '配置项 language 只能是 en 或 zh-CN，而不是 {value}。',
  'error.noConnectorResource': '没有连接器服务资源 {resource}。',
  'error.noConnectorPlatform': '没有连接器服务平台 {platform}。',
  'error.noConnectorPackage': '没有连接器包服务平台 {platform}。',
  'error.claimIdentity': 'HTTP 调用方不能冒用网关身份。',
  'error.decide': '网关只转发消息，不做发送决策。',
  'error.noContext': '网关不消费模型上下文。',
};

export type GatewayTextKey = keyof typeof en;

export const t = createTranslator({ en, 'zh-CN': zh });
