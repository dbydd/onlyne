import type { Bot, CharacterCard, ChatMessage, Conversation, Lorebook, LorebookEntry, SceneBlockConfig, SceneConfig } from './contracts.ts';
import { matchLorebook } from './character.ts';

export interface SceneBlock { id: string; title: string; role: 'system' | 'user' | 'assistant'; position: 'before-character' | 'after-character' | 'before-example' | 'after-example' | 'at-depth' | 'after-history'; depth: number; order: number; enabled: boolean; content: string; source: string }
export interface CompiledScene {
  system: string;
  prompt: string;
  blocks: SceneBlock[];
  matchedLore: string[];
  stats: { promptTokens: number; loreTokens: number; historyMessages: number; droppedLore: number; droppedForBudget: number };
}

const estimate = (text: string): number => Math.ceil(text.length / 3.5);
const section = (title: string, body: string): string => body.trim().length === 0 ? '' : `## ${title}\n${body.trim()}\n`;

/** Budgeted lore selection: constants first, then order, each entry counted against chars. */
export function selectLore(lorebook: Lorebook | undefined, scanText: string, budgetChars: number): { entries: LorebookEntry[]; dropped: number } {
  const matched = matchLorebook(lorebook, scanText);
  const entries: LorebookEntry[] = [];
  let used = 0;
  let dropped = 0;
  for (const entry of matched) {
    const cost = entry.content.length + 8;
    if (used + cost > budgetChars) { dropped += 1; continue; }
    used += cost;
    entries.push(entry);
  }
  return { entries, dropped };
}

function personaText(conversation: Conversation): string {
  if (conversation.personaName === undefined && conversation.personaDescription === undefined) return '';
  return `${conversation.personaName ?? 'The user'}${conversation.personaDescription === undefined ? '' : `: ${conversation.personaDescription}`}`;
}

function characterSections(character: CharacterCard | undefined): string {
  if (character === undefined) return '';
  return [
    section('Description', character.description),
    section('Personality', character.personality ?? ''),
    section('Scenario', character.scenario ?? ''),
    section('Example dialogue', character.exampleDialogue ?? ''),
  ].join('');
}

function sceneBlocks(bot: Bot, conversation: Conversation): SceneBlock[] {
  const character = bot.character;
  const blocks: SceneBlock[] = [];
  const add = (block: Partial<SceneBlock> & Pick<SceneBlock, 'id' | 'content' | 'source'>): void => {
    blocks.push({ title: block.title ?? block.id, role: block.role ?? 'system', position: block.position ?? 'after-character', depth: block.depth ?? 4, order: block.order ?? 100, enabled: block.enabled ?? true, ...block });
  };
  if (character?.systemPrompt !== undefined) add({ id: 'character.system', title: 'Character system prompt', content: character.systemPrompt, source: 'character', order: 10 });
  if (character?.description !== undefined) add({ id: 'character.description', title: 'Character description', content: character.description, source: 'character', order: 20 });
  if (character?.personality !== undefined) add({ id: 'character.personality', title: 'Character personality', content: character.personality, source: 'character', order: 30 });
  if (character?.scenario !== undefined) add({ id: 'character.scenario', title: 'Scenario', content: character.scenario, source: 'character', order: 40 });
  const persona = personaText(conversation);
  if (persona.length > 0) add({ id: 'persona', title: 'User persona', content: persona, source: 'persona', order: 50 });
  if (conversation.authorNote !== undefined && conversation.authorNote.trim().length > 0) {
    add({ id: 'author-note', title: "Author's note", content: conversation.authorNote, source: 'author-note', position: 'at-depth', depth: conversation.scene?.authorNoteDepth ?? 4, role: conversation.scene?.authorNoteRole ?? 'system', order: 60 });
  }
  if (character?.exampleDialogue !== undefined) add({ id: 'character.example', title: 'Example dialogue', content: character.exampleDialogue, source: 'character', position: 'before-example', order: 70 });
  if (character?.postHistoryInstructions !== undefined) add({ id: 'character.post-history', title: 'Post-history instructions', content: character.postHistoryInstructions, source: 'character', position: 'after-history', order: 900 });
  const overrides = conversation.scene?.blocks ?? [];
  const byId = new Map<string, SceneBlockConfig>(overrides.map(block => [block.id, block]));
  const merged = blocks.map(block => {
    const override = byId.get(block.id);
    return override === undefined ? block : { ...block, ...override, title: override.title ?? block.title, content: override.content.length > 0 ? override.content : block.content };
  });
  for (const block of overrides) {
    if (merged.some(item => item.id === block.id)) continue;
    merged.push({
      id: block.id, title: block.title ?? block.id, role: block.role ?? 'system',
      position: block.position ?? 'after-character', depth: block.depth ?? 4,
      order: block.order ?? 100, enabled: block.enabled ?? true, content: block.content, source: 'override',
    });
  }
  const rank: Record<SceneBlock['position'], number> = {
    'before-character': 0, 'after-character': 1, 'before-example': 2, 'after-example': 3,
    'at-depth': 4, 'after-history': 5,
  };
  return merged.filter(block => block.enabled).sort((left, right) =>
    rank[left.position] - rank[right.position] || left.order - right.order);
}

/**
 * Compiles one turn exactly as the harness sees it. OMP keeps the conversation itself, so the
 * prompt carries only the scene blocks, matched lore, persona and notes; history is injected when
 * the caller asks for a stateless transcript (group scenes and regeneration).
 */
export function compileScene(
  bot: Bot,
  conversation: Conversation,
  messages: ChatMessage[],
  userText: string,
  options: { includeHistory?: boolean; greeting?: string; members?: Bot[] } = {},
): CompiledScene {
  const scene: SceneConfig = conversation.scene ?? {};
  const budgetChars = scene.worldInfoBudget ?? 4000;
  const scanDepth = scene.worldInfoScanDepth ?? 6;
  const scanText = [userText, ...messages.slice(-scanDepth).map(message => message.text)].join('\n');
  const lore = selectLore(bot.lorebook, scanText, budgetChars);
  let selectedLore = lore.entries.map(entry => entry.content);
  const blocks = sceneBlocks(bot, conversation);
  // The character card's static half already rides in the runtime system prompt; repeating it
  // here would spend tokens twice. The prompt keeps the volatile half: lore, persona, examples,
  // notes and post-history instructions.
  const staticIds: Record<string, true> = {
    'character.system': true, 'character.description': true, 'character.personality': true, 'character.scenario': true,
  };
  const fixed = blocks.filter(block => block.position !== 'at-depth' && block.position !== 'after-history' && staticIds[block.id] !== true);
  const atDepth = blocks.filter(block => block.position === 'at-depth');
  const afterHistory = blocks.filter(block => block.position === 'after-history');

  const buildSystem = (): string => [
    section('Character', bot.name),
    bot.instructions.trim(),
    characterSections(bot.character),
    section('World information', selectedLore.join('\n\n')),
    section('Memory', bot.memory),
    section('Scene memory', conversation.memory),
  ].filter(part => part.trim().length > 0).join('\n');

  const transcript = options.includeHistory === true ? messages.slice(-(scene.historyLimit ?? 30)) : [];
  const depthNote = atDepth.map(block => block.content).join('\n\n');
  const splitAt = Math.max(0, transcript.length - Math.max(...atDepth.map(block => block.depth), 0));
  let history = transcript.map((message, index) => {
    const member = options.members?.find(item => item.id === message.botId);
    const speaker = message.botId === undefined ? 'User' : message.botId === bot.id ? bot.name : member?.name ?? message.botId;
    const line = `${speaker}: ${message.text}`;
    return index === splitAt && depthNote.length > 0 ? `${depthNote}\n\n${line}` : line;
  });
  if (depthNote.length > 0 && history.length === 0) history.push(depthNote);

  const opener = conversation.greeting !== undefined && conversation.greeted !== true ? options.greeting ?? conversation.greeting : undefined;
  const renderPrompt = (): string => [
    'Continue the scene as the character described above. Stay in character, answer the latest user message, and keep continuity with the existing session transcript.',
    opener === undefined ? '' : `The scene opened with your greeting, already shown to the user:\n${opener}`,
    fixed.map(block => block.content).join('\n\n'),
    history.length === 0 ? '' : `Scene transcript (most recent last):\n${history.join('\n')}`,
    depthNote.length > 0 && history.length === 0 ? depthNote : '',
    afterHistory.map(block => block.content).join('\n\n'),
    `User: ${userText}`,
  ].filter(part => part.trim().length > 0).join('\n\n');
  let prompt = renderPrompt();
  let droppedForBudget = 0;
  const budget = scene.promptBudget;
  while (budget !== undefined && budget > 0 && estimate(prompt) > budget) {
    if (history.length > 0) history = history.slice(1);
    else if (selectedLore.length > 1) { selectedLore = selectedLore.slice(0, -1); droppedForBudget += 1; }
    else break;
    prompt = renderPrompt();
  }
  const loreTokens = estimate(selectedLore.join('\n\n'));
  const system = buildSystem();
  return {
    system, prompt, blocks,
    matchedLore: lore.entries.map(entry => entry.id).slice(0, lore.entries.length - droppedForBudget),
    stats: { promptTokens: estimate(prompt), loreTokens, historyMessages: history.length, droppedLore: lore.dropped, droppedForBudget },
  };
}
