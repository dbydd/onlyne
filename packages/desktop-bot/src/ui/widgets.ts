import type { Json, RpcFrame, WidgetEnvelope } from '../contracts.ts';
import type {
  WidgetContext,
  WidgetHost,
  WidgetHostOptions,
  WidgetRegistry,
  WidgetRenderer,
  WidgetRenderContext,
} from './api.ts';

/** setTimeout wraps to 1 ms when the delay exceeds a signed 32-bit int. */
const MAX_TIMER_MS = 2_147_483_647;

type Placement = 'aboveEditor' | 'belowEditor';

interface AttentionItem {
  key: string;
  element: HTMLElement;
  context: WidgetContext;
  dialogId?: string;
  timer?: ReturnType<typeof setTimeout>;
  settled: boolean;
}

interface TextWidget {
  lines: string[];
  placement: Placement;
}

interface ChatUiState {
  key: string;
  items: Map<string, AttentionItem>;
  inlineViews: Map<string, AttentionItem>;
  statuses: Map<string, HTMLElement>;
  widgets: Map<string, TextWidget>;
  title?: string;
}

interface AskOption {
  label: string;
  description?: string;
  preview?: string;
}

interface AskQuestion {
  id: string;
  question: string;
  header?: string;
  options: AskOption[];
  multi: boolean;
  recommended?: number;
}

interface AskQuestionRefs {
  question: AskQuestion;
  inputs: HTMLInputElement[];
  custom: HTMLInputElement;
}

interface DialogShell {
  root: HTMLElement;
  body: HTMLElement;
  actions: HTMLElement;
}

function chatKey(context: WidgetContext): string {
  return `${context.conversationId}\u0000${context.botId}`;
}

function displayText(value: unknown, fallback: string): string {
  return typeof value === 'string' && value.length > 0 ? value : fallback;
}

function stringArray(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((entry): entry is string => typeof entry === 'string') : [];
}

/** RPC payloads are untyped on the wire; every field is read and validated by the caller. */
function field(value: unknown, key: string): unknown {
  return value !== null && typeof value === 'object' ? Reflect.get(value, key) : undefined;
}

function timeoutOf(frame: RpcFrame): number | undefined {
  const value = frame.timeout;
  if (typeof value !== 'number' || !Number.isFinite(value) || value <= 0) return undefined;
  return Math.min(value, MAX_TIMER_MS);
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function dom<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const element = document.createElement(tag);
  element.className = className;
  if (text !== undefined) element.textContent = text;
  return element;
}

function actionButton(label: string, primary: boolean, onClick: () => void): HTMLButtonElement {
  const button = dom('button', primary ? 'onlyne-button onlyne-button-primary' : 'onlyne-button', label);
  button.type = 'button';
  button.addEventListener('click', onClick);
  return button;
}

function dialogShell(method: string, title: string): DialogShell {
  const root = dom('section', 'onlyne-dialog');
  root.dataset.method = method;
  root.setAttribute('role', 'dialog');
  root.setAttribute('aria-label', title);
  const body = dom('div', 'onlyne-dialog-body');
  const actions = dom('div', 'onlyne-dialog-actions');
  root.append(dom('div', 'onlyne-dialog-title', title), body, actions);
  return { root, body, actions };
}

function reply(id: string, body: Record<string, unknown>): RpcFrame {
  return { type: 'extension_ui_response', id, ...body };
}

function rawContent(content: Json): HTMLElement {
  return dom('pre', 'onlyne-widget-raw', JSON.stringify(content, null, 2));
}

function unsupportedView(view: WidgetEnvelope, context: WidgetRenderContext): HTMLElement {
  const root = dom('div', 'onlyne-widget-unknown');
  root.dataset.protocol = view.protocol;
  const protocol = displayText(view.protocol, '(missing)');
  root.append(
    dom('div', 'onlyne-widget-notice', `No renderer is registered for widget protocol '${protocol}'. Raw content:`),
    rawContent(view.content),
  );
  const actions = dom('div', 'onlyne-dialog-actions');
  actions.append(actionButton('Dismiss', false, () => void context.cancel()));
  root.append(actions);
  return root;
}

function rendererFailure(view: WidgetEnvelope, error: unknown, context: WidgetRenderContext): HTMLElement {
  const root = dom('div', 'onlyne-widget-unknown onlyne-widget-error');
  root.dataset.protocol = view.protocol;
  root.append(
    dom('div', 'onlyne-widget-notice', `Widget protocol '${view.protocol}' could not render: ${errorMessage(error)}`),
    rawContent(view.content),
  );
  const actions = dom('div', 'onlyne-dialog-actions');
  actions.append(actionButton('Dismiss', false, () => void context.cancel()));
  root.append(actions);
  return root;
}

/** Registries are shared with renderer plugins; a disposer restores the handler it displaced. */
export function createWidgetRegistry(): WidgetRegistry {
  const stacks = new Map<string, WidgetRenderer[]>();
  return {
    register(protocol: string, renderer: WidgetRenderer): () => void {
      if (!protocol) throw new Error('A widget protocol needs a non-empty name');
      if (typeof renderer !== 'function') throw new Error(`The renderer for '${protocol}' must be a function`);
      let stack = stacks.get(protocol);
      if (!stack) {
        stack = [];
        stacks.set(protocol, stack);
      }
      stack.push(renderer);
      return () => {
        const index = stack.lastIndexOf(renderer);
        if (index < 0) return;
        stack.splice(index, 1);
        if (stack.length === 0) stacks.delete(protocol);
      };
    },
    render(view: WidgetEnvelope, context: WidgetRenderContext): HTMLElement {
      const stack = stacks.get(view.protocol);
      const renderer = stack?.[stack.length - 1];
      if (!renderer) return unsupportedView(view, context);
      let element: HTMLElement;
      try {
        element = renderer(view.content, context);
      } catch (error) {
        return rendererFailure(view, error, context);
      }
      if (!(element instanceof HTMLElement)) {
        return rendererFailure(view, new Error('the renderer returned no element'), context);
      }
      return element;
    },
    protocols(): string[] {
      return [...stacks.keys()];
    },
  };
}

function parseQuestions(value: unknown): AskQuestion[] {
  if (!Array.isArray(value)) throw new Error('no questions array');
  return value.map((raw, index) => {
    if (!raw || typeof raw !== 'object') throw new Error(`question ${index} is not an object`);
    const id = field(raw, 'id');
    if (typeof id !== 'string' || !id) throw new Error(`question ${index} has no id`);
    const text = field(raw, 'question');
    if (typeof text !== 'string') throw new Error(`question '${id}' has no text`);
    const declared = field(raw, 'options');
    const rawOptions = Array.isArray(declared) ? declared : [];
    const options = rawOptions.map((entry, optionIndex) => {
      if (!entry || typeof entry !== 'object') {
        throw new Error(`question '${id}' option ${optionIndex} is not an object`);
      }
      const label = field(entry, 'label');
      if (typeof label !== 'string') {
        throw new Error(`question '${id}' option ${optionIndex} has no label`);
      }
      const description = field(entry, 'description');
      const preview = field(entry, 'preview');
      return {
        label,
        description: typeof description === 'string' ? description : undefined,
        preview: typeof preview === 'string' ? preview : undefined,
      } satisfies AskOption;
    });
    const recommended = field(raw, 'recommended');
    const header = field(raw, 'header');
    return {
      id,
      question: text,
      header: typeof header === 'string' && header ? header : undefined,
      options,
      multi: field(raw, 'multi') === true,
      recommended:
        typeof recommended === 'number' &&
        Number.isInteger(recommended) &&
        recommended >= 0 &&
        recommended < options.length
          ? recommended
          : undefined,
    } satisfies AskQuestion;
  });
}

/** One host per renderer: all state is keyed by bot + conversation, not by the visible chat. */
export function createWidgetHost(options: WidgetHostOptions): WidgetHost {
  const chats = new Map<string, ChatUiState>();
  const dialogs = new Map<string, AttentionItem>();
  let activeKey: string | undefined;
  let disposed = false;
  let sequence = 0;

  function chatFor(context: WidgetContext): ChatUiState {
    const key = chatKey(context);
    let chat = chats.get(key);
    if (!chat) {
      chat = { key, items: new Map(), inlineViews: new Map(), statuses: new Map(), widgets: new Map() };
      chats.set(key, chat);
    }
    return chat;
  }

  function placeItem(chat: ChatUiState, item: AttentionItem): void {
    chat.items.set(item.key, item);
    if (item.dialogId) dialogs.set(item.dialogId, item);
    if (activeKey === undefined) {
      // A session_start dialog can arrive before the renderer picks a chat; show it instead of holding it.
      activeKey = chat.key;
      paint(chat);
      return;
    }
    if (chat.key === activeKey) options.dialogs.append(item.element);
  }

  function dropItem(chat: ChatUiState, key: string): void {
    const item = chat.items.get(key);
    if (!item) return;
    chat.items.delete(key);
    if (item.dialogId) dialogs.delete(item.dialogId);
    item.element.remove();
  }

  function placeInlineView(chat: ChatUiState, item: AttentionItem): void {
    chat.inlineViews.set(item.key, item);
    if (chat.key === activeKey) options.inlineViews.append(item.element);
  }

  function dropInlineView(chat: ChatUiState, key: string): void {
    const item = chat.inlineViews.get(key);
    if (!item) return;
    chat.inlineViews.delete(key);
    item.element.remove();
  }

  function settleInline(chat: ChatUiState, item: AttentionItem): void {
    if (item.settled) return;
    item.settled = true;
    clearTimeout(item.timer);
    dropInlineView(chat, item.key);
  }

  function send(context: WidgetContext, response: RpcFrame): void {
    void Promise.resolve()
      .then(() => options.respond(context, response))
      .catch((error: unknown) => options.notify(`RPC UI response failed: ${errorMessage(error)}`, 'error'));
  }

  function settle(item: AttentionItem, response?: RpcFrame): void {
    if (item.settled) return;
    item.settled = true;
    clearTimeout(item.timer);
    const chat = chats.get(chatKey(item.context));
    if (chat) dropItem(chat, item.key);
    else item.element.remove();
    if (response && !disposed) send(item.context, response);
  }

  function openDialog(
    chat: ChatUiState,
    id: string,
    element: HTMLElement,
    context: WidgetContext,
    timeout: number | undefined,
  ): AttentionItem {
    const stale = dialogs.get(id);
    if (stale) settle(stale);
    const item: AttentionItem = { key: `dialog:${id}`, dialogId: id, element, context, settled: false };
    placeItem(chat, item);
    if (timeout !== undefined) {
      item.timer = setTimeout(() => settle(item, reply(id, { cancelled: true, timedOut: true })), timeout);
    }
    return item;
  }

  function showError(chat: ChatUiState, context: WidgetContext, message: string): void {
    const card = dom('section', 'onlyne-dialog onlyne-dialog-error');
    card.dataset.method = 'error';
    card.setAttribute('role', 'alert');
    const item: AttentionItem = {
      key: `error:${++sequence}`,
      element: card,
      context,
      settled: false,
    };
    const actions = dom('div', 'onlyne-dialog-actions');
    actions.append(actionButton('Dismiss', false, () => settle(item)));
    card.append(
      dom('div', 'onlyne-dialog-title', 'RPC UI error'),
      dom('p', 'onlyne-dialog-message', message),
      actions,
    );
    placeItem(chat, item);
    options.notify(message, 'error');
  }

  function showSelect(chat: ChatUiState, context: WidgetContext, frame: RpcFrame): void {
    const id = typeof frame.id === 'string' ? frame.id : undefined;
    if (!id) return showError(chat, context, 'A select request arrived without an id.');
    const { root, body, actions } = dialogShell('select', displayText(frame.title, 'Select an option'));
    const labels = stringArray(frame.options);
    const details = Array.isArray(frame.optionDetails) ? frame.optionDetails : [];
    let item: AttentionItem | undefined;
    const cancel = () => {
      if (item) settle(item, reply(id, { cancelled: true }));
    };
    if (labels.length === 0) body.append(dom('div', 'onlyne-dialog-empty', 'This request offered no options.'));
    labels.forEach((label, index) => {
      const button = dom('button', 'onlyne-dialog-option');
      button.type = 'button';
      button.dataset.index = String(index);
      button.append(dom('span', 'onlyne-dialog-option-label', label));
      const description = field(details[index], 'description');
      if (typeof description === 'string' && description) {
        button.append(dom('span', 'onlyne-dialog-option-description', description));
      }
      button.addEventListener('click', () => {
        if (item) settle(item, reply(id, { value: label }));
      });
      body.append(button);
    });
    actions.append(actionButton('Cancel', false, cancel));
    root.addEventListener('keydown', (event) => {
      if (event.key === 'Escape') {
        event.preventDefault();
        cancel();
      }
    });
    item = openDialog(chat, id, root, context, timeoutOf(frame));
  }

  function showConfirm(chat: ChatUiState, context: WidgetContext, frame: RpcFrame): void {
    const id = typeof frame.id === 'string' ? frame.id : undefined;
    if (!id) return showError(chat, context, 'A confirm request arrived without an id.');
    const { root, body, actions } = dialogShell('confirm', displayText(frame.title, 'Confirm'));
    body.append(dom('p', 'onlyne-dialog-message', typeof frame.message === 'string' ? frame.message : ''));
    let item: AttentionItem | undefined;
    const answer = (confirmed: boolean) => {
      if (item) settle(item, reply(id, { confirmed }));
    };
    actions.append(actionButton('Cancel', false, () => answer(false)));
    actions.append(actionButton('Confirm', true, () => answer(true)));
    root.addEventListener('keydown', (event) => {
      if (event.key === 'Escape') {
        event.preventDefault();
        answer(false);
      }
    });
    item = openDialog(chat, id, root, context, timeoutOf(frame));
  }

  function showInput(chat: ChatUiState, context: WidgetContext, frame: RpcFrame): void {
    const id = typeof frame.id === 'string' ? frame.id : undefined;
    if (!id) return showError(chat, context, 'An input request arrived without an id.');
    const { root, body, actions } = dialogShell('input', displayText(frame.title, 'Input'));
    const secret = frame.secret === true;
    const input = dom('input', 'onlyne-dialog-input');
    input.type = secret ? 'password' : 'text';
    input.autocomplete = 'off';
    if (typeof frame.placeholder === 'string') input.placeholder = frame.placeholder;
    input.setAttribute('aria-label', displayText(frame.title, 'Input'));
    const field = dom('div', 'onlyne-dialog-field');
    field.append(input);
    if (secret) field.append(dom('div', 'onlyne-dialog-hint', 'Characters are hidden as you type.'));
    body.append(field);
    let item: AttentionItem | undefined;
    const submit = () => {
      if (item) settle(item, reply(id, { value: input.value }));
    };
    input.addEventListener('keydown', (event) => {
      if (event.key === 'Enter') {
        event.preventDefault();
        submit();
      }
    });
    actions.append(
      actionButton('Cancel', false, () => {
        if (item) settle(item, reply(id, { cancelled: true }));
      }),
    );
    actions.append(actionButton('Send', true, submit));
    root.addEventListener('keydown', (event) => {
      if (event.key === 'Escape') {
        event.preventDefault();
        if (item) settle(item, reply(id, { cancelled: true }));
      }
    });
    item = openDialog(chat, id, root, context, timeoutOf(frame));
    if (chat.key === activeKey) input.focus();
  }

  function showEditor(chat: ChatUiState, context: WidgetContext, frame: RpcFrame): void {
    const id = typeof frame.id === 'string' ? frame.id : undefined;
    if (!id) return showError(chat, context, 'An editor request arrived without an id.');
    const { root, body, actions } = dialogShell('editor', displayText(frame.title, 'Editor'));
    const editor = dom('textarea', 'onlyne-dialog-editor');
    editor.value = typeof frame.prefill === 'string' ? frame.prefill : '';
    editor.rows = 8;
    editor.spellcheck = false;
    editor.setAttribute('aria-label', displayText(frame.title, 'Editor'));
    if (frame.promptStyle === true) {
      root.classList.add('onlyne-dialog-prompt');
      root.dataset.promptStyle = 'true';
    }
    body.append(
      editor,
      dom('div', 'onlyne-dialog-hint', 'Ctrl+Enter or Cmd+Enter submits. Enter starts a new line.'),
    );
    let item: AttentionItem | undefined;
    const submit = () => {
      if (item) settle(item, reply(id, { value: editor.value }));
    };
    const cancel = () => {
      if (item) settle(item, reply(id, { cancelled: true }));
    };
    editor.addEventListener('keydown', (event) => {
      if (event.key === 'Enter' && (event.ctrlKey || event.metaKey)) {
        event.preventDefault();
        submit();
      }
      if (event.key === 'Escape') {
        event.preventDefault();
        cancel();
      }
    });
    actions.append(actionButton('Cancel', false, cancel));
    actions.append(actionButton('Submit', true, submit));
    // The wire gives editor no timeout; a host timer would cut off a long edit.
    item = openDialog(chat, id, root, context, undefined);
    if (chat.key === activeKey) editor.focus();
  }

  function showAsk(chat: ChatUiState, context: WidgetContext, frame: RpcFrame): void {
    const id = typeof frame.id === 'string' ? frame.id : undefined;
    if (!id) return showError(chat, context, 'An ask request arrived without an id.');
    let questions: AskQuestion[];
    try {
      questions = parseQuestions(frame.questions);
    } catch (error) {
      showError(chat, context, `An ask request was malformed: ${errorMessage(error)}.`);
      send(context, reply(id, { cancelled: true }));
      return;
    }
    if (questions.length === 0) {
      showError(chat, context, 'An ask request contained no questions.');
      send(context, reply(id, { cancelled: true }));
      return;
    }
    const { root, body, actions } = dialogShell('ask', 'Questions');
    const refs: AskQuestionRefs[] = [];
    questions.forEach((question, questionIndex) => {
      const fieldset = dom('fieldset', 'onlyne-ask-question');
      fieldset.dataset.questionId = question.id;
      if (question.header) fieldset.append(dom('legend', 'onlyne-ask-header', question.header));
      fieldset.append(dom('div', 'onlyne-ask-text', question.question));
      const list = dom('div', 'onlyne-ask-options');
      const inputs: HTMLInputElement[] = [];
      question.options.forEach((option, optionIndex) => {
        const label = dom('label', 'onlyne-ask-option');
        const control = dom('input', question.multi ? 'onlyne-ask-checkbox' : 'onlyne-ask-radio');
        control.type = question.multi ? 'checkbox' : 'radio';
        control.name = `onlyne-ask-${id}-${questionIndex}`;
        control.value = option.label;
        control.dataset.index = String(optionIndex);
        const text = dom('span', 'onlyne-ask-option-body');
        text.append(dom('span', 'onlyne-ask-option-label', option.label));
        if (question.recommended === optionIndex) {
          text.append(dom('span', 'onlyne-ask-recommended', 'Recommended'));
        }
        if (option.description) text.append(dom('span', 'onlyne-ask-description', option.description));
        if (option.preview) text.append(dom('pre', 'onlyne-ask-preview', option.preview));
        label.append(control, text);
        list.append(label);
        inputs.push(control);
      });
      const custom = dom('input', 'onlyne-ask-custom');
      custom.type = 'text';
      custom.placeholder = 'Or type your own answer';
      custom.setAttribute('aria-label', `Own answer for ${question.question}`);
      if (!question.multi) {
        // A single-select answer cannot carry both a chosen option and custom input.
        custom.addEventListener('input', () => {
          if (custom.value) inputs.forEach((input) => (input.checked = false));
        });
        inputs.forEach((input) => input.addEventListener('change', () => (custom.value = '')));
      }
      fieldset.append(list, custom);
      body.append(fieldset);
      refs.push({ question, inputs, custom });
    });
    let item: AttentionItem | undefined;
    const submit = () => {
      if (!item) return;
      const answers = refs.map(({ question, inputs, custom }) => {
        const selectedOptions: string[] = [];
        inputs.forEach((input, index) => {
          const option = question.options[index];
          if (!option || !input.checked) return;
          if (!selectedOptions.includes(option.label)) selectedOptions.push(option.label);
        });
        const answer: { id: string; selectedOptions: string[]; customInput?: string } = {
          id: question.id,
          selectedOptions,
        };
        const customInput = custom.value.trim();
        if (customInput && (question.multi || selectedOptions.length === 0)) answer.customInput = customInput;
        return answer;
      });
      settle(item, reply(id, { answers }));
    };
    const cancel = () => {
      if (item) settle(item, reply(id, { cancelled: true }));
    };
    actions.append(actionButton('Cancel', false, cancel));
    actions.append(actionButton('Submit answers', true, submit));
    root.addEventListener('keydown', (event) => {
      if (event.key === 'Escape') {
        event.preventDefault();
        cancel();
      }
    });
    item = openDialog(chat, id, root, context, timeoutOf(frame));
  }

  function cancelDialog(frame: RpcFrame): void {
    const targetId = typeof frame.targetId === 'string' ? frame.targetId : undefined;
    if (!targetId) return;
    const item = dialogs.get(targetId);
    if (!item) return;
    // The RPC side already resolved this dialog; a response would be ignored.
    settle(item);
  }

  function showUrlCard(chat: ChatUiState, context: WidgetContext, frame: RpcFrame, url: string): void {
    const id = typeof frame.id === 'string' ? frame.id : `url-${++sequence}`;
    const key = `url:${id}`;
    const existing = chat.items.get(key);
    if (existing) dropItem(chat, key);
    const card = dom('section', 'onlyne-url-card');
    card.dataset.method = 'open_url';
    card.setAttribute('role', 'group');
    card.setAttribute('aria-label', 'Open link');
    const item: AttentionItem = { key, element: card, context, settled: false };
    const copy = (value: string, label: string) => {
      const clipboard = navigator.clipboard;
      if (!clipboard) {
        options.notify(`Copy is unavailable; select the ${label} manually.`, 'error');
        return;
      }
      void clipboard.writeText(value).then(
        () => options.notify(`${label} copied.`, 'info'),
        (error: unknown) => options.notify(`Copy failed: ${errorMessage(error)}`, 'error'),
      );
    };
    card.append(dom('div', 'onlyne-dialog-title', 'Open link'));
    if (typeof frame.instructions === 'string' && frame.instructions) {
      card.append(dom('p', 'onlyne-dialog-message', frame.instructions));
    }
    const link = dom('a', 'onlyne-url-link', url);
    link.setAttribute('role', 'link');
    link.tabIndex = 0;
    const open = () => options.openUrl(url);
    link.addEventListener('click', open);
    link.addEventListener('keydown', (event) => {
      if (event.key === 'Enter' || event.key === ' ') {
        event.preventDefault();
        open();
      }
    });
    const linkRow = dom('div', 'onlyne-url-row');
    linkRow.append(link, actionButton('Copy', false, () => copy(url, 'Link')));
    card.append(linkRow);
    if (typeof frame.launchUrl === 'string' && frame.launchUrl) {
      const launchUrl = frame.launchUrl;
      const row = dom('div', 'onlyne-url-row');
      row.append(
        dom('div', 'onlyne-dialog-hint', 'Short link (recommended for copying):'),
        dom('code', 'onlyne-url-launch', launchUrl),
        actionButton('Copy short link', false, () => copy(launchUrl, 'Short link')),
      );
      card.append(row);
    }
    const actions = dom('div', 'onlyne-dialog-actions');
    actions.append(
      actionButton('Dismiss', false, () => settle(item)),
      actionButton('Open in browser', true, open),
    );
    card.append(actions);
    placeItem(chat, item);
  }

  function paintWidgets(chat: ChatUiState): void {
    if (chat.key !== activeKey) return;
    const paintMount = (mount: HTMLElement, placement: Placement): void => {
      const entries = [...chat.widgets.entries()].filter(([, widget]) => widget.placement === placement);
      mount.replaceChildren(...entries.map(([key, widget]) => {
        const box = dom('div', 'onlyne-widget');
        box.dataset.key = key;
        for (const line of widget.lines) box.append(dom('div', 'onlyne-widget-line', line));
        return box;
      }));
    };
    paintMount(options.widgetsAbove, 'aboveEditor');
    paintMount(options.widgetsBelow, 'belowEditor');
  }

  function paint(chat: ChatUiState): void {
    options.dialogs.replaceChildren(...[...chat.items.values()].map((item) => item.element));
    options.inlineViews.replaceChildren(...[...chat.inlineViews.values()].map((item) => item.element));
    options.status.replaceChildren(...chat.statuses.values());
    paintWidgets(chat);
    if (chat.title !== undefined) options.setTitle(chat.title);
  }

  function renderView(
    view: WidgetEnvelope,
    chat: ChatUiState,
    context: WidgetContext,
    settle: (targetChat: ChatUiState, item: AttentionItem) => void,
  ): HTMLElement {
    const wrapper = dom('section', 'onlyne-view');
    wrapper.dataset.protocol = view.protocol;
    wrapper.dataset.viewId = view.id;
    wrapper.dataset.interactive = String(view.interactive === true);
    if (view.title) wrapper.append(dom('div', 'onlyne-view-title', view.title));
    const item: AttentionItem = { key: `view:${view.id}`, element: wrapper, context, settled: false };
    const settleView = async (answer: Json | undefined, cancelled: boolean): Promise<void> => {
      if (item.settled) return;
      item.settled = true;
      settle(chat, item);
      try {
        await options.respondView(view.id, cancelled ? undefined : answer, cancelled);
      } catch (error) {
        options.notify(`Widget response failed: ${errorMessage(error)}`, 'error');
      }
    };
    const renderContext: WidgetRenderContext = {
      view,
      respond: (answer) => settleView(answer, false),
      cancel: () => settleView(undefined, true),
    };
    try {
      wrapper.append(options.registry.render(view, renderContext));
    } catch (error) {
      wrapper.append(dom('div', 'onlyne-view-error', `Widget '${view.protocol}' could not render: ${errorMessage(error)}`));
      if (view.interactive) wrapper.append(actionButton('Dismiss', false, () => void settleView(undefined, true)));
    }
    if (!view.interactive) {
      const dismiss = dom('button', 'onlyne-view-dismiss', 'Dismiss');
      dismiss.type = 'button';
      dismiss.addEventListener('click', () => settle(chat, item));
      wrapper.append(dismiss);
    }
    return wrapper;
  }

  function showUnknownMethod(chat: ChatUiState, context: WidgetContext, frame: RpcFrame, method: string): void {
    const id = typeof frame.id === 'string' ? frame.id : undefined;
    showError(chat, context, `Unsupported RPC UI method '${displayText(method, '(missing)')}'.`);
    // An unanswered dialog id would hang the RPC turn; unknown ids are ignored server-side.
    if (id) send(context, reply(id, { cancelled: true }));
  }

  return {
    handleRpc(frame: RpcFrame, incoming: WidgetContext): void {
      if (disposed || frame.type !== 'extension_ui_request') return;
      // Snapshot the scope: the caller may reuse its object, and every answer must route back exactly.
      const context: WidgetContext = { conversationId: incoming.conversationId, botId: incoming.botId };
      const chat = chatFor(context);
      switch (frame.method) {
        case 'select':
          return showSelect(chat, context, frame);
        case 'confirm':
          return showConfirm(chat, context, frame);
        case 'input':
          return showInput(chat, context, frame);
        case 'editor':
          return showEditor(chat, context, frame);
        case 'ask':
          return showAsk(chat, context, frame);
        case 'cancel':
          return cancelDialog(frame);
        case 'notify': {
          if (typeof frame.message !== 'string' || !frame.message) {
            return options.notify('A notify request arrived without a message.', 'error');
          }
          return options.notify(frame.message, displayText(frame.notifyType, 'info'));
        }
        case 'setStatus': {
          const key = typeof frame.statusKey === 'string' ? frame.statusKey : undefined;
          if (!key) return showError(chat, context, 'A setStatus request arrived without a statusKey.');
          const text = typeof frame.statusText === 'string' ? frame.statusText : undefined;
          if (text === undefined) {
            const element = chat.statuses.get(key);
            if (element) {
              chat.statuses.delete(key);
              element.remove();
            }
            return;
          }
          let element = chat.statuses.get(key);
          if (!element) {
            element = dom('span', 'onlyne-status');
            element.dataset.key = key;
            chat.statuses.set(key, element);
            if (chat.key === activeKey) options.status.append(element);
          }
          element.textContent = text;
          return;
        }
        case 'setWidget': {
          const key = typeof frame.widgetKey === 'string' ? frame.widgetKey : undefined;
          if (!key) return showError(chat, context, 'A setWidget request arrived without a widgetKey.');
          if (frame.widgetLines === undefined) {
            chat.widgets.delete(key);
            paintWidgets(chat);
            return;
          }
          if (!Array.isArray(frame.widgetLines)) {
            return showError(chat, context, 'A setWidget request carried lines that are not a string array.');
          }
          const placement: Placement = frame.widgetPlacement === 'belowEditor' ? 'belowEditor' : 'aboveEditor';
          chat.widgets.set(key, { lines: stringArray(frame.widgetLines), placement });
          paintWidgets(chat);
          return;
        }
        case 'setTitle': {
          if (typeof frame.title !== 'string') {
            return options.notify('A setTitle request arrived without a title.', 'error');
          }
          chat.title = frame.title;
          if (chat.key === activeKey) options.setTitle(frame.title);
          return;
        }
        case 'set_editor_text': {
          if (typeof frame.text !== 'string') {
            return options.notify('A set_editor_text request arrived without text.', 'error');
          }
          // The composer belongs to the selected chat; a background chat must not overwrite it.
          if (chat.key === activeKey) options.setEditorText(frame.text);
          return;
        }
        case 'open_url': {
          if (typeof frame.url !== 'string' || !frame.url) {
            return options.notify('An open_url request arrived without a url.', 'error');
          }
          return showUrlCard(chat, context, frame, frame.url);
        }
        default:
          return showUnknownMethod(chat, context, frame, typeof frame.method === 'string' ? frame.method : '');
      }
    },
    handleView(view: WidgetEnvelope): void {
      if (disposed) return;
      const context: WidgetContext = { conversationId: view.conversationId, botId: view.botId };
      const chat = chatFor(context);
      const key = `view:${view.id}`;
      if (chat.items.has(key)) dropItem(chat, key);
      if (chat.inlineViews.has(key)) dropInlineView(chat, key);
      if (view.state !== 'open') return;
      const element = renderView(
        view,
        chat,
        context,
        view.interactive ? (targetChat, item) => dropItem(targetChat, item.key) : settleInline,
      );
      if (view.interactive) placeItem(chat, { key, element, context, settled: false });
      else placeInlineView(chat, { key, element, context, settled: false });
    },
    select(context: WidgetContext): void {
      if (disposed) return;
      activeKey = chatKey(context);
      const chat = chats.get(activeKey);
      if (!chat) {
        options.dialogs.replaceChildren();
        options.inlineViews.replaceChildren();
        options.status.replaceChildren();
        options.widgetsAbove.replaceChildren();
        options.widgetsBelow.replaceChildren();
        return;
      }
      paint(chat);
    },
    dispose(): void {
      if (disposed) return;
      disposed = true;
      for (const chat of chats.values()) {
        for (const item of chat.items.values()) {
          item.settled = true;
          clearTimeout(item.timer);
          item.element.remove();
        }
        for (const item of chat.inlineViews.values()) {
          item.settled = true;
          item.element.remove();
        }
      }
      chats.clear();
      dialogs.clear();
      activeKey = undefined;
      options.dialogs.replaceChildren();
      options.inlineViews.replaceChildren();
      options.status.replaceChildren();
      options.widgetsAbove.replaceChildren();
      options.widgetsBelow.replaceChildren();
    },
  };
}
