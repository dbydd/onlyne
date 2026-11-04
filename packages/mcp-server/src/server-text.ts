import { createTranslator } from '@onlyne/node-host/i18n';

const en = {
  'cli.usage': 'usage: onlyne-mcp-server <config.json>',
  'startup.pending': 'The Onlyne MCP server is starting.',
  'config.unreadable': 'The configuration file is unreadable: {path}',
  'config.object': 'The configuration needs {field} as an object.',
  'config.string': 'The configuration field {field} is a non-empty string.',
  'config.language': 'The configuration language is en or zh-CN, not {value}.',
  'config.positiveInt': 'The configuration field {field} is a positive integer.',
  'error.claimIdentity': 'HTTP callers cannot claim the node identity.',
  'error.decide': 'The MCP server exposes typed tools; it does not decide msg() sends.',
  'error.noContext': 'The MCP server consumes no model context of its own.',
  'error.unknownTool': 'Unknown tool {name}.',
  'error.unknownResource': 'Unknown resource {uri}.',
  'context.summary': 'Delivery into the Onlyne MCP server node; a reply continues in the conversation thread.',
} as const;

const zh: Record<keyof typeof en, string> = {
  'cli.usage': '用法：onlyne-mcp-server <config.json>',
  'startup.pending': 'Onlyne MCP 服务器正在启动。',
  'config.unreadable': '配置文件无法读取：{path}',
  'config.object': '配置项 {field} 需要是一个对象。',
  'config.string': '配置项 {field} 需要是非空字符串。',
  'config.language': '配置项 language 只能是 en 或 zh-CN，而不是 {value}。',
  'config.positiveInt': '配置项 {field} 需要是正整数。',
  'error.claimIdentity': 'HTTP 调用方不能冒用节点身份。',
  'error.decide': 'MCP 服务器暴露类型化工具，不通过 msg() 做发送决策。',
  'error.noContext': 'MCP 服务器自己不消费模型上下文。',
  'error.unknownTool': '未知工具 {name}。',
  'error.unknownResource': '未知资源 {uri}。',
  'context.summary': '投递到 Onlyne MCP 服务器节点；回复继续落在该会话 thread。',
};

export type ServerTextKey = keyof typeof en;

export const t = createTranslator({ en, 'zh-CN': zh });
