import type {
  Bot, ChatMessage, Conversation, DesktopController, DesktopState, HostTool, HostToolContext, ToolResult,
} from './contracts.ts';

const OPERATIONS = [
  'list_bots', 'create_bot', 'update_bot', 'create_group', 'update_group',
  'configure_group', 'post_to_group', 'message_bot', 'remember', 'recall', 'stop',
] as const;
type Operation = (typeof OPERATIONS)[number];
const THINKING_LEVELS = ['inherit', 'off', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max'] as const;

const record = (value: unknown, field: string): Record<string, unknown> => {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) throw new Error(`${field} must be an object`);
  return value as Record<string, unknown>;
};
const requireText = (value: unknown, field: string): string => {
  if (typeof value !== 'string' || value.trim().length === 0) throw new Error(`${field} must be nonempty text`);
  return value;
};
const stringValue = (value: unknown, field: string): string => {
  if (typeof value !== 'string') throw new Error(`${field} must be text`);
  return value;
};
const stringList = (value: unknown, field: string): string[] => {
  if (!Array.isArray(value) || value.length === 0) throw new Error(`${field} must be a nonempty array of nonempty strings`);
  return value.map(item => requireText(item, field));
};
const modelChoice = (value: unknown): { provider: string; id: string } | null => {
  if (value === null) return null;
  const model = record(value, 'model');
  return { provider: requireText(model.provider, 'model.provider'), id: requireText(model.id, 'model.id') };
};
const thinkingLevel = (value: unknown): string => {
  const level = requireText(value, 'thinking');
  if (!(THINKING_LEVELS as readonly string[]).includes(level)) throw new Error(`thinking must be one of ${THINKING_LEVELS.join(', ')}`);
  return level;
};
const memoryScope = (value: unknown): 'bot' | 'group' => {
  const scope = requireText(value, 'scope');
  if (scope !== 'bot' && scope !== 'group') throw new Error('scope must be bot or group');
  return scope;
};
const result = (value: unknown): ToolResult => ({ content: [{ type: 'text', text: JSON.stringify(value, null, 2) }] });

const resolveBot = (state: DesktopState, selector: string): Bot => {
  const exact = state.bots.find(item => item.id === selector);
  if (exact !== undefined) return exact;
  const named = state.bots.filter(item => item.name === selector);
  if (named.length === 1) return named[0]!;
  if (named.length > 1) throw new Error(`Bot name is ambiguous: ${selector}. Use the bot id.`);
  const similar = state.bots.filter(item => item.name.toLowerCase() === selector.toLowerCase());
  if (similar.length === 1) return similar[0]!;
  if (similar.length > 1) throw new Error(`Bot name is ambiguous: ${selector}. Use the bot id.`);
  throw new Error(`Bot not found: ${selector}`);
};
const resolveConversation = (state: DesktopState, id: string): Conversation => {
  const conversation = state.conversations.find(item => item.id === id);
  if (conversation === undefined) throw new Error(`Conversation not found: ${id}`);
  return conversation;
};
const botView = (state: DesktopState, bot: Bot, selfId: string) => ({
  id: bot.id, name: bot.name, description: bot.description,
  team_admin: bot.id === state.teamAdminId, self: bot.id === selfId,
});
const conversationView = (state: DesktopState, conversation: Conversation) => ({
  id: conversation.id, title: conversation.title, kind: conversation.kind, admin_bot_id: conversation.adminBotId,
  turn_mode: conversation.scene?.group?.mode ?? 'manual',
  auto_turns: conversation.scene?.group?.auto === true,
  max_auto_turns: conversation.scene?.group?.maxAutoTurns ?? 8,
  bots: conversation.botIds.map(id => {
    const bot = state.bots.find(item => item.id === id);
    return { id, name: bot?.name ?? id, team_admin: id === state.teamAdminId };
  }),
});
const capabilityIds = (state: DesktopState, value: unknown): string[] => {
  const ids = stringList(value, 'capabilities');
  const known = new Set(state.capabilities.map(item => item.id));
  for (const id of ids) if (!known.has(id)) throw new Error(`Unknown capability: ${id}`);
  return [...new Set(ids)];
};

const parameters: Record<string, unknown> = {
  type: 'object',
  properties: {
    operation: {
      type: 'string', enum: [...OPERATIONS],
      description: 'Team operation. Required arguments: list_bots none; create_bot name; update_bot bot_id and at least one changed field; create_group title and bot_ids; update_group group_id and at least one of title/bot_ids/admin_bot_id; configure_group group_id and auto_turns or max_auto_turns; post_to_group group_id and text; message_bot bot_id and text; remember scope and text (add replace to overwrite a passage); recall scope; stop bot_id.',
    },
    bot_id: { type: 'string', description: 'Bot id, or a unique bot name. Required by update_bot, message_bot and stop.' },
    bot_ids: { type: 'array', items: { type: 'string' }, description: 'Group members as bot ids or unique names. Required by create_group; replaces the member list in update_group.' },
    group_id: { type: 'string', description: 'Group conversation id. Required by update_group and post_to_group.' },
    conversation_id: { type: 'string', description: 'Conversation id for stop. Defaults to the current conversation; stopping elsewhere requires the team administrator or that conversation admin.' },
    name: { type: 'string', description: 'create_bot: display name (required). update_bot: new display name.' },
    title: { type: 'string', description: 'create_group: group title (required). update_group: new title.' },
    description: { type: 'string', description: 'Short role summary visible to teammates through list_bots.' },
    instructions: { type: 'string', description: 'Bot instructions.' },
    text: {
      type: 'string',
      description: 'post_to_group and message_bot: message body. remember: memory text to append, or the replacement text when replace is given (empty clears the replaced passage).',
    },
    to: { type: 'array', items: { type: 'string' }, description: 'post_to_group recipients as bot ids or unique names. Defaults to every other member of the group.' },
    scope: { type: 'string', enum: ['bot', 'group'], description: 'remember and recall memory scope: your own bot memory, or the current group memory.' },
    replace: { type: 'string', description: 'remember: exact passage of the current memory to replace with text. Omit to append.' },
    model: {
      type: ['object', 'null'],
      properties: { provider: { type: 'string' }, id: { type: 'string' } },
      required: ['provider', 'id'], additionalProperties: false,
      description: 'Model choice for the bot. null clears it.',
    },
    thinking: { type: 'string', enum: [...THINKING_LEVELS], description: 'Thinking level for the bot.' },
    capabilities: { type: 'array', items: { type: 'string' }, description: 'Capability ids to attach.' },
    runtime: { type: 'string', description: 'Runtime driver id such as omp-rpc.' },
    runtime_options: { type: 'object', additionalProperties: true, description: 'Runtime driver options.' },
    workspace: { type: 'string', description: 'Bot workspace path.' },
    admin_bot_id: { type: 'string', description: 'Group admin as bot id or unique name. Defaults to the caller when it is a member, otherwise the first member.' },
    auto_turns: { type: 'boolean', description: 'configure_group: let the group keep answering after each reply, advancing through the turn order.' },
    max_auto_turns: { type: 'integer', minimum: 1, description: 'configure_group: consecutive bot replies before waiting for a person.' },
  },
  required: ['operation'],
  additionalProperties: false,
};

export function createTeamTools(controller: DesktopController): HostTool[] {
  const mutate = async <T>(context: HostToolContext, action: string, payload: Record<string, unknown>): Promise<T> => {
    if (context.signal.aborted) throw new Error('Team operation cancelled');
    return await controller.command(action, payload) as T;
  };

  return [{
    name: 'desktop_team',
    label: 'Desktop team',
    description: 'Coordinate the local bot team: list_bots, create_bot, update_bot, create_group, update_group, configure_group, post_to_group, message_bot, remember, recall and stop. The calling identity comes from the conversation, never from arguments. The team administrator manages the roster and groups; a bot may edit itself, configure a group it administers, post to groups it belongs to, message a bot it shares a conversation with, and read or write its own memory and the current group memory. Messages between bots go through the desktop inbox and each conversation keeps its own runtime session; the Onlyne network stays on msg. Read desktop://scene for the current scene, desktop://bots/<id> for a teammate card, desktop://health for runtime errors.',
    parameters,
    async execute(args, context) {
      const requested = requireText(args.operation, 'operation');
      if (!(OPERATIONS as readonly string[]).includes(requested)) throw new Error(`Unknown team operation: ${requested} (expected one of ${OPERATIONS.join(', ')})`);
      const operation = requested as Operation;
      if (context.signal.aborted) throw new Error('Team operation cancelled');
      const state = controller.snapshot();
      const caller = state.bots.find(item => item.id === context.bot.id);
      if (caller === undefined) throw new Error('The calling bot is not part of this team');
      const current = state.conversations.find(item => item.id === context.conversation.id);
      if (current === undefined) throw new Error('The current conversation no longer exists');
      const managesTeam = state.teamAdminId === caller.id;

      switch (operation) {
        case 'list_bots': {
          const groups = state.conversations
            .filter(item => item.kind === 'group' && (managesTeam || item.botIds.includes(caller.id)))
            .map(item => ({ ...conversationView(state, item), member: item.botIds.includes(caller.id) }));
          return result({
            you: { id: caller.id, name: caller.name, team_admin: managesTeam, conversation: conversationView(state, current) },
            bots: state.bots.map(bot => botView(state, bot, caller.id)),
            groups,
          });
        }
        case 'create_bot': {
          if (!managesTeam) throw new Error('Only the team administrator can manage the team');
          const bot: Record<string, unknown> = { name: requireText(args.name, 'name') };
          if (args.bot_id !== undefined) {
            const id = requireText(args.bot_id, 'bot_id');
            if (state.bots.some(item => item.id === id)) throw new Error(`Bot already exists: ${id}`);
            bot.id = id;
          }
          if (args.description !== undefined) bot.description = stringValue(args.description, 'description');
          if (args.instructions !== undefined) bot.instructions = stringValue(args.instructions, 'instructions');
          if (args.model !== undefined) bot.model = modelChoice(args.model);
          if (args.thinking !== undefined) bot.thinking = thinkingLevel(args.thinking);
          if (args.capabilities !== undefined) bot.capabilities = capabilityIds(state, args.capabilities);
          if (args.runtime !== undefined) bot.runtime = requireText(args.runtime, 'runtime');
          if (args.runtime_options !== undefined) bot.runtimeOptions = record(args.runtime_options, 'runtime_options');
          if (args.workspace !== undefined) bot.workspace = requireText(args.workspace, 'workspace');
          const created = await mutate<Bot>(context, 'bot.save', { bot });
          const conversation = controller.snapshot().conversations.find(item => item.kind === 'direct' && item.botIds.includes(created.id));
          return result({ bot: botView(controller.snapshot(), created, caller.id), conversation_id: conversation?.id ?? null });
        }
        case 'update_bot': {
          const target = resolveBot(state, requireText(args.bot_id, 'bot_id'));
          if (!managesTeam && target.id !== caller.id) throw new Error('A bot can only update itself; only the team administrator can update another bot');
          const bot: Record<string, unknown> = { id: target.id };
          let changes = 0;
          if (args.name !== undefined) { bot.name = requireText(args.name, 'name'); changes += 1; }
          if (args.description !== undefined) { bot.description = stringValue(args.description, 'description'); changes += 1; }
          if (args.instructions !== undefined) { bot.instructions = stringValue(args.instructions, 'instructions'); changes += 1; }
          if (args.model !== undefined) { bot.model = modelChoice(args.model); changes += 1; }
          if (args.thinking !== undefined) { bot.thinking = thinkingLevel(args.thinking); changes += 1; }
          if (args.capabilities !== undefined) { bot.capabilities = capabilityIds(state, args.capabilities); changes += 1; }
          if (args.runtime !== undefined) { bot.runtime = requireText(args.runtime, 'runtime'); changes += 1; }
          if (args.runtime_options !== undefined) { bot.runtimeOptions = record(args.runtime_options, 'runtime_options'); changes += 1; }
          if (args.workspace !== undefined) { bot.workspace = requireText(args.workspace, 'workspace'); changes += 1; }
          if (changes === 0) throw new Error('update_bot needs at least one field to change');
          const updated = await mutate<Bot>(context, 'bot.save', { bot });
          return result({ bot: botView(controller.snapshot(), updated, caller.id) });
        }
        case 'create_group': {
          if (!managesTeam) throw new Error('Only the team administrator can manage the team');
          const title = requireText(args.title, 'title');
          const botIds = [...new Set(stringList(args.bot_ids, 'bot_ids').map(value => resolveBot(state, value).id))];
          const adminBotId = args.admin_bot_id === undefined
            ? (botIds.includes(caller.id) ? caller.id : botIds[0]!)
            : resolveBot(state, requireText(args.admin_bot_id, 'admin_bot_id')).id;
          if (!botIds.includes(adminBotId)) throw new Error('admin_bot_id must be a group member');
          const conversation = await mutate<Conversation>(context, 'conversation.create', { title, botIds, adminBotId, kind: 'group' });
          return result({ group: conversationView(controller.snapshot(), conversation) });
        }
        case 'update_group': {
          const group = resolveConversation(state, requireText(args.group_id, 'group_id'));
          if (group.kind !== 'group') throw new Error('update_group targets a group conversation');
          if (!managesTeam && group.adminBotId !== caller.id) throw new Error('Only the group admin or the team administrator can update this group');
          if (args.title === undefined && args.bot_ids === undefined && args.admin_bot_id === undefined) {
            throw new Error('update_group needs title, bot_ids or admin_bot_id');
          }
          const botIds = args.bot_ids === undefined
            ? group.botIds
            : [...new Set(stringList(args.bot_ids, 'bot_ids').map(value => resolveBot(state, value).id))];
          const adminBotId = args.admin_bot_id === undefined
            ? group.adminBotId
            : resolveBot(state, requireText(args.admin_bot_id, 'admin_bot_id')).id;
          if (!botIds.includes(adminBotId)) throw new Error('admin_bot_id must be a group member');
          const title = args.title === undefined ? group.title : requireText(args.title, 'title');
          const updated = await mutate<Conversation>(context, 'conversation.update', { id: group.id, title, botIds, adminBotId, kind: 'group' });
          return result({ group: conversationView(controller.snapshot(), updated) });
        }
        case 'configure_group': {
          const group = resolveConversation(state, requireText(args.group_id, 'group_id'));
          if (group.kind !== 'group') throw new Error('configure_group targets a group conversation');
          if (!managesTeam && group.adminBotId !== caller.id) throw new Error('Only the group admin or the team administrator can configure this group');
          if (args.auto_turns === undefined && args.max_auto_turns === undefined) {
            throw new Error('configure_group needs auto_turns or max_auto_turns');
          }
          if (args.auto_turns !== undefined && typeof args.auto_turns !== 'boolean') throw new Error('auto_turns must be boolean');
          const limit = args.max_auto_turns === undefined ? group.scene?.group?.maxAutoTurns ?? 8 : Number(args.max_auto_turns);
          if (!Number.isInteger(limit) || limit < 1) throw new Error('max_auto_turns must be an integer of at least 1');
          const previous = group.scene?.group ?? {};
          const scene = {
            ...(group.scene ?? {}),
            group: {
              ...previous,
              mode: args.auto_turns === true ? 'round-robin' : previous.mode ?? 'manual',
              auto: args.auto_turns === undefined ? previous.auto === true : args.auto_turns,
              maxAutoTurns: limit,
            },
          };
          const updated = await mutate<Conversation>(context, 'conversation.update', { id: group.id, scene });
          return result({ group: conversationView(controller.snapshot(), updated) });
        }
        case 'post_to_group': {
          const group = resolveConversation(state, requireText(args.group_id, 'group_id'));
          if (group.kind !== 'group') throw new Error('post_to_group targets a group conversation');
          if (!managesTeam && !group.botIds.includes(caller.id)) throw new Error('Post only to a group you belong to, or ask the team administrator');
          const text = requireText(args.text, 'text');
          const requested = args.to === undefined ? undefined : stringList(args.to, 'to');
          const recipients = requested === undefined
            ? group.botIds.filter(id => id !== caller.id)
            : [...new Set(requested.map(value => {
              const target = resolveBot(state, value);
              if (!group.botIds.includes(target.id)) throw new Error(`${target.name} is not a member of this group`);
              return target.id;
            }))];
          if (recipients.length === 0) throw new Error('post_to_group has no recipients');
          const message = await mutate<ChatMessage>(context, 'message.send', { conversationId: group.id, text, botIds: recipients });
          return result({ conversation_id: group.id, message_id: message.id, delivered_to: recipients });
        }
        case 'message_bot': {
          const target = resolveBot(state, requireText(args.bot_id, 'bot_id'));
          if (target.id === caller.id) throw new Error('message_bot targets another bot');
          const text = requireText(args.text, 'text');
          const shares = (item: Conversation): boolean => item.botIds.includes(caller.id) && item.botIds.includes(target.id);
          let conversation: Conversation | undefined;
          if (current.kind === 'group' && shares(current)) conversation = current;
          else conversation = state.conversations.find(item => item.kind === 'group' && shares(item)) ?? state.conversations.find(shares);
          // A bot's direct conversation is the only place it can be prompted without a shared group; the team administrator may use it.
          if (conversation === undefined && managesTeam) {
            conversation = state.conversations.find(item => item.kind === 'direct' && item.botIds.length === 1 && item.botIds[0] === target.id);
          }
          if (conversation === undefined) throw new Error(`No shared conversation with ${target.name}; ask the team administrator to create a group`);
          const message = await mutate<ChatMessage>(context, 'message.send', { conversationId: conversation.id, text, botIds: [target.id] });
          return result({ conversation_id: conversation.id, message_id: message.id, delivered_to: [target.id] });
        }
        case 'remember': {
          const scope = memoryScope(args.scope);
          if (scope === 'group' && current.kind !== 'group') throw new Error('Group memory is only available inside a group conversation');
          const text = stringValue(args.text, 'text');
          const replace = args.replace === undefined ? undefined : requireText(args.replace, 'replace');
          if (text.length === 0 && replace === undefined) throw new Error('remember needs text or replace');
          const field = scope === 'bot' ? caller.memory : current.memory;
          if (replace !== undefined && !field.includes(replace)) throw new Error('replace text is not present in the current memory');
          const payload: Record<string, unknown> = scope === 'bot' ? { botId: caller.id } : { conversationId: current.id };
          payload.text = text;
          if (replace !== undefined) payload.replace = replace;
          await mutate<unknown>(context, 'memory.save', payload);
          const after = controller.snapshot();
          const memory = scope === 'bot'
            ? after.bots.find(item => item.id === caller.id)?.memory ?? ''
            : after.conversations.find(item => item.id === current.id)?.memory ?? '';
          return result({ scope, memory });
        }
        case 'recall': {
          const scope = memoryScope(args.scope);
          if (scope === 'group' && current.kind !== 'group') throw new Error('Group memory is only available inside a group conversation');
          return result({ scope, memory: scope === 'bot' ? caller.memory : current.memory });
        }
        case 'stop': {
          const target = resolveBot(state, requireText(args.bot_id, 'bot_id'));
          const conversation = args.conversation_id === undefined
            ? current
            : resolveConversation(state, requireText(args.conversation_id, 'conversation_id'));
          if (!conversation.botIds.includes(target.id)) throw new Error(`${target.name} is not a member of that conversation`);
          if (!managesTeam && conversation.id !== current.id) {
            if (!conversation.botIds.includes(caller.id)) throw new Error('Stop only inside a conversation you belong to');
            if (conversation.adminBotId !== caller.id) throw new Error('Only the team administrator or the group admin can stop a bot outside the current conversation');
          }
          await mutate<unknown>(context, 'runtime.stop', { botId: target.id, conversationId: conversation.id });
          return result({ stopped: true, bot_id: target.id, conversation_id: conversation.id });
        }
        default: throw new Error(`Unknown team operation: ${String(operation)}`);
      }
    },
  }];
}
