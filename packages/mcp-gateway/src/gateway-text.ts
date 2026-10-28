import { createTranslator } from '@onlyne/node-host/i18n';

const en = {
  'cli.usage': 'usage: onlyne-mcp-gateway <config.json>',
  'startup.pending': 'The gateway is starting.',
  'config.unreadable': 'The configuration file is unreadable: {path}',
  'config.object': 'The configuration needs {field} as an object.',
  'config.string': 'The configuration field {field} is a non-empty string.',
  'config.servers': 'The configuration needs servers as an array.',
  'config.duplicateName': 'Two servers share the name {name}.',
  'config.language': 'The configuration language is en or zh-CN, not {value}.',
  'config.stringList': 'The configuration field {field} needs a list of non-empty strings.',
  'config.reservedName': 'The server name {name} uses the reserved resource prefix {prefix}.',
  'config.mountsControllers': 'mounts.controllers needs at least one node id.',
  'config.mountsOwner': 'The node owner {owner} cannot be its own mount controller.',
  'config.mountsCommands': 'mounts.commands needs an array.',
  'config.mountsTools': 'mounts.tools needs an array.',
  'config.mountsToolServer': 'mounts.tools names the server {server}, which is not configured.',
  'config.mountsDuplicate': 'Two mounts entries share the {kind} {id}.',
  'config.mountTick': 'mounts.tick_ms is an integer from 100 to 60000.',
  'error.claimIdentity': 'HTTP callers cannot claim the gateway identity.',
  'error.decide': 'The gateway forwards tool calls; it decides nothing.',
  'error.noContext': 'The gateway consumes no model context.',
} as const;

const zh: Record<keyof typeof en, string> = {
  'cli.usage': '用法：onlyne-mcp-gateway <config.json>',
  'startup.pending': '网关正在启动。',
  'config.unreadable': '配置文件无法读取：{path}',
  'config.object': '配置项 {field} 需要是一个对象。',
  'config.string': '配置项 {field} 需要是非空字符串。',
  'config.servers': '配置项 servers 需要是一个数组。',
  'config.duplicateName': '有两个服务器共用了名称 {name}。',
  'config.language': '配置项 language 只能是 en 或 zh-CN，而不是 {value}。',
  'config.stringList': '配置项 {field} 需要是非空字符串列表。',
  'config.reservedName': '服务器名称 {name} 占用了保留的资源前缀 {prefix}。',
  'config.mountsControllers': 'mounts.controllers 至少需要一个节点标识。',
  'config.mountsOwner': '节点自身 {owner} 不能作为自己的挂载控制者。',
  'config.mountsCommands': 'mounts.commands 需要是一个数组。',
  'config.mountsTools': 'mounts.tools 需要是一个数组。',
  'config.mountsToolServer': 'mounts.tools 指向的服务器 {server} 并未配置。',
  'config.mountsDuplicate': '有两个挂载配置项共用了 {kind} {id}。',
  'config.mountTick': 'mounts.tick_ms 是 100 到 60000 之间的整数。',
  'error.claimIdentity': 'HTTP 调用方不能冒用网关身份。',
  'error.decide': '网关只转发工具调用，不做任何决策。',
  'error.noContext': '网关不消费模型上下文。',
};

export type GatewayTextKey = keyof typeof en;

export const t = createTranslator({ en, 'zh-CN': zh });
