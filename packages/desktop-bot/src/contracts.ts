export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type RpcFrame = { type: string; id?: string; [key: string]: unknown };
export type ThinkingLevel = 'inherit' | 'off' | 'minimal' | 'low' | 'medium' | 'high' | 'xhigh' | 'max';

export interface ModelChoice { provider: string; id: string }
export type MediaKind = 'image' | 'video' | 'audio' | 'svg' | 'sticker' | 'file';
export interface MediaItem {
  id: string;
  kind: MediaKind;
  contentType: string;
  name: string;
  size: number;
  createdAt: number;
  botId?: string;
  url?: string;
}
export interface CharacterCard {
  name?: string;
  description: string;
  personality?: string;
  scenario?: string;
  systemPrompt?: string;
  postHistoryInstructions?: string;
  exampleDialogue?: string;
  firstMessage?: string;
  alternateGreetings?: string[];
  creatorNotes?: string;
  tags?: string[];
  avatar?: string;
  source?: 'sillytavern' | 'manual';
}
export interface LorebookEntry {
  id: string;
  keys: string[];
  secondaryKeys?: string[];
  content: string;
  enabled?: boolean;
  constant?: boolean;
  order?: number;
  depth?: number;
  role?: 'system' | 'user' | 'assistant';
}
export interface Lorebook { name?: string; entries: LorebookEntry[] }
export interface SceneBlockConfig {
  id: string;
  title?: string;
  content: string;
  enabled?: boolean;
  order?: number;
  role?: 'system' | 'user' | 'assistant';
  position?: 'before-character' | 'after-character' | 'before-example' | 'after-example' | 'at-depth' | 'after-history';
  depth?: number;
}
export interface SceneConfig {
  blocks?: SceneBlockConfig[];
  authorNoteDepth?: number;
  authorNoteRole?: 'system' | 'user' | 'assistant';
  worldInfoBudget?: number;
  worldInfoScanDepth?: number;
  historyLimit?: number;
  promptBudget?: number;
  group?: { mode?: 'manual' | 'round-robin'; order?: string[]; auto?: boolean; maxAutoTurns?: number };
}
export const DEFAULT_MAX_AUTO_TURNS = 8;
export interface RuntimeOptions {
  command?: string;
  args?: string[];
  env?: Record<string, string>;
}
export interface Bot {
  id: string;
  name: string;
  description: string;
  instructions: string;
  memory: string;
  workspace: string;
  model?: ModelChoice;
  thinking: ThinkingLevel;
  capabilities: string[];
  runtime: string;
  runtimeOptions: RuntimeOptions;
  configDir?: string;
  onlyne?: { nodeId: string; peers: string[] };
  plugins?: { host: string[]; renderer: string[] };
  character?: CharacterCard;
  lorebook?: Lorebook;
  createdAt: number;
  updatedAt: number;
}
export interface Conversation {
  id: string;
  title: string;
  kind: 'direct' | 'group';
  botIds: string[];
  adminBotId: string;
  memory: string;
  personaName?: string;
  personaDescription?: string;
  authorNote?: string;
  greeting?: string;
  greeted?: boolean;
  scene?: SceneConfig;
  createdAt: number;
  updatedAt: number;
}
export interface ChatMessage {
  id: string;
  conversationId: string;
  role: 'user' | 'assistant' | 'tool' | 'system';
  botId?: string;
  text: string;
  content?: Json[];
  usage?: Record<string, number>;
  media?: MediaItem[];
  createdAt: number;
}
export type Capability =
  | { id: string; name: string; kind: 'mcp'; config: Record<string, Json> }
  | { id: string; name: string; kind: 'skill'; path: string }
  | { id: string; name: string; kind: 'extension'; path: string };
export interface DesktopSettings {
  theme: string;
  locale: 'en' | 'zh-CN';
  hostPlugins: string[];
  rendererPlugins: string[];
  customCss: string;
  nodeId: string;
  peers: string[];
}
export interface DesktopState {
  version: 1;
  bots: Bot[];
  /** The bot allowed to manage the team. Absent only while a legacy state is being normalized. */
  teamAdminId?: string;
  conversations: Conversation[];
  messages: ChatMessage[];
  capabilities: Capability[];
  media: MediaItem[];
  settings: DesktopSettings;
}
export interface DesktopStore {
  read(): DesktopState;
  update<T>(change: (state: DesktopState) => T): Promise<T>;
  close(): Promise<void>;
}

export interface RuntimeContext {
  bot: Bot;
  conversation: Conversation;
  capabilities: Capability[];
  dataDir: string;
  sessionDir: string;
  systemPrompt?: string;
  toolNames?: string[];
  onFrame(frame: RpcFrame): void;
  /** Available before startup finishes, so extension dialogs raised during session_start can be answered. */
  onEarlySend?(send: (frame: RpcFrame) => void): void;
}
export interface RuntimeConnection {
  request(command: RpcFrame): Promise<RpcFrame>;
  send(frame: RpcFrame): void;
  close(): Promise<void>;
}
export interface RuntimeDriver {
  id: string;
  connect(context: RuntimeContext): Promise<RuntimeConnection>;
}
export interface DesktopEvent {
  type: string;
  conversationId?: string;
  botId?: string;
  [key: string]: unknown;
}
export interface ToolResult {
  content: Array<{ type: string; [key: string]: unknown }>;
  details?: unknown;
}
export interface HostToolContext {
  bot: Bot;
  conversation: Conversation;
  signal: AbortSignal;
  update(result: ToolResult): void;
}
export interface HostTool {
  name: string;
  label: string;
  description: string;
  parameters: Record<string, unknown>;
  execute(args: Record<string, unknown>, context: HostToolContext): Promise<ToolResult>;
}
export interface HostUri {
  scheme: string;
  description: string;
  writable?: boolean;
  immutable?: boolean;
  read(url: string, context: HostToolContext): Promise<{ content: string; contentType?: string }>;
  write?(url: string, content: string, context: HostToolContext): Promise<void>;
}
export interface DesktopPluginApi {
  registerRuntime(driver: RuntimeDriver): void;
  registerTool(tool: HostTool): void;
  registerUri(uri: HostUri): void;
  emit(event: DesktopEvent): void;
}
export interface DesktopPlugin {
  id: string;
  setup(api: DesktopPluginApi): void | Promise<void>;
  dispose?(): void | Promise<void>;
}
export type DesktopPluginFactory = (context: { bot: Bot; dataDir: string }) => DesktopPlugin | Promise<DesktopPlugin>;

export interface WidgetView {
  id: string;
  protocol: string;
  title?: string;
  content: Json;
  interactive?: boolean;
}
export interface WidgetEnvelope extends WidgetView {
  conversationId: string;
  botId: string;
  requestId: string;
  state: 'open' | 'answered' | 'cancelled';
  answer?: Json;
}
export interface OnlyneBridge {
  status(): Promise<unknown>;
  dispatch(input: { delivery_kind: string; msg: string; meta?: Record<string, unknown> }, context: HostToolContext): Promise<unknown>;
  read(uri: string): Promise<{ content: string; contentType?: string }>;
  action(action: string, args: Record<string, unknown>): Promise<unknown>;
  close(): Promise<void>;
}
export interface OnlyneBridgeOptions {
  dataDir: string;
  nodeId: string;
  peers: string[];
  contexts(): Array<{ alias: string; summary: string; state: string }>;
  inbound(entry: unknown, context?: string): Promise<void>;
  decide(input: unknown): Promise<unknown>;
  emit(event: DesktopEvent): void;
}

export interface DesktopController {
  snapshot(): DesktopState;
  subscribe(listener: (event: DesktopEvent) => void): () => void;
  command(action: string, payload: Record<string, unknown>): Promise<unknown>;
  close(): Promise<void>;
}
export interface ServerOptions {
  controller: DesktopController;
  hostname?: string;
  port?: number;
  rendererPlugins?: string[];
  botRenderers?: Map<string, string[]>;
  media?: (id: string) => Promise<{ path: string; contentType: string } | undefined>;
}
export interface DesktopServer {
  url: string;
  port: number;
  close(): Promise<void>;
}
