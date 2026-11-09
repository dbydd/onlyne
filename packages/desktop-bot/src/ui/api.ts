import type { DesktopState, DesktopEvent, RpcFrame, WidgetEnvelope, Json } from '../contracts.ts';
import type { DeskLayout, WorkspaceGroup } from './layout.ts';

export interface WorkspaceContribution {
  id: string;
  title: string;
  hint?: string;
  shortcut?: string;
  group?: WorkspaceGroup;
}

export interface WidgetContext {
  conversationId: string;
  botId: string;
}
export interface WidgetRenderContext {
  view: WidgetEnvelope;
  respond(answer: Json): Promise<void>;
  cancel(): Promise<void>;
}
export type WidgetRenderer = (content: Json, context: WidgetRenderContext) => HTMLElement;
export interface WidgetRegistry {
  register(protocol: string, renderer: WidgetRenderer): () => void;
  render(view: WidgetEnvelope, context: WidgetRenderContext): HTMLElement;
  protocols(): string[];
}
export interface WidgetHostOptions {
  dialogs: HTMLElement;
  /** Mount for transient widgets that the runtime requests above the editor. */
  widgetsAbove: HTMLElement;
  /** Mount for transient widgets that the runtime requests below the editor. */
  widgetsBelow: HTMLElement;
  /** Mount for rich, non-interactive views rendered in the conversation surface. */
  inlineViews: HTMLElement;
  status: HTMLElement;
  registry: WidgetRegistry;
  respond(context: WidgetContext, response: RpcFrame): Promise<void>;
  respondView(id: string, answer: Json | undefined, cancelled: boolean): Promise<void>;
  notify(message: string, level?: string): void;
  setEditorText(text: string): void;
  setTitle(title: string): void;
  openUrl(url: string): void;
}
export interface WidgetHost {
  handleRpc(frame: RpcFrame, context: WidgetContext): void;
  handleView(view: WidgetEnvelope): void;
  select(context: WidgetContext): void;
  dispose(): void;
}
export interface DesktopUiApi {
  command<T = unknown>(action: string, payload?: Record<string, unknown>): Promise<T>;
  state(): DesktopState;
  activeContext(): WidgetContext | undefined;
  subscribe(listener: (event: DesktopEvent) => void): () => void;
  widgets: WidgetRegistry;
  registerPanel(id: string, title: string, render: (state: DesktopState) => HTMLElement): () => void;
  /** Puts a panel on the rail when its group is not `studio`. Studio panels stay in the library and the command palette. */
  registerWorkspace(entry: WorkspaceContribution): () => void;
  layout: {
    get(): DeskLayout;
    update(change: (layout: DeskLayout) => DeskLayout): void;
    reset(): void;
  };
  registerMessageRenderer(role: string, render: (message: unknown) => HTMLElement): () => void;
  registerTheme(id: string, css: string): () => void;
  setTheme(id: string): void;
}
declare global {
  interface Window { onlyne: DesktopUiApi }
}
