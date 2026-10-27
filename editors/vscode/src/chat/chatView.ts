import { randomUUID } from 'node:crypto';
import * as vscode from 'vscode';
import type { ChatMessage, Event } from '../client';
import { EventSocket } from '../client/ws';
import { describeError } from '../errors';
import { log } from '../log';
import type { Services } from '../services';
import type { ChatHostToWebview, ChatViewState, ChatWebviewToHost } from '../shared/protocol';
import { safePost } from '../shared/safePost';
import { cleanText } from '../shared/untrustedText';
import { webviewHtml, webviewOptions } from '../shared/webviewHtml';
import { chatGroupId, foldChat, wikilinksIn, type ChatRow } from './chatModel';
import { createConversation, recentConversations, type Conversation } from './conversations';

/** The skill whose events the runner answers in chat mode (the chat corpus). */
export const CHAT_SKILL = 'chat';
const HISTORY_LIMIT = 200;
const MAX_QUESTION = 8000;
/** The runner reads at most this many focus pages. */
const MAX_FOCUS = 10;

/**
 * The Chat view (SPEC §3.11): a WebviewView beside Runs in the secondary sidebar. A conversation is
 * a `conversation` instance with a page-bound chat; a question is `append_message` (the person's
 * turn) plus a `chat` event the runner answers, as the asker, with one assistant turn. Live over
 * `/ws` (`event_subscribe {chat_group_id}`); progress comes from the question's own lineage.
 *
 * The host holds the state and every id: the webview gets cleaned rows and opaque keys, and what it
 * sends back is checked against what the host offered (SPEC §5: webviews hold no token, and an id
 * from a webview is never trusted).
 */
export class ChatViewProvider implements vscode.WebviewViewProvider, vscode.Disposable {
  static readonly viewType = 'escurel.chat';

  private view: vscode.WebviewView | undefined;
  private viewSub: vscode.Disposable | undefined;
  private subject: string | undefined;
  private status: ChatViewState['status'] = 'ready';
  private message: string | undefined;
  private recent: Conversation[] = [];
  /** The open chat: a conversation, or a record's own page chat (`record`). */
  private current: (Conversation & { group: string; record?: true }) | undefined;
  /** Pages pinned as context for the next question; sent as `provenance.focus`. */
  private focus: { pageId: string; title: string }[] = [];
  private messages: ChatMessage[] = [];
  private sending = false;
  private progress: string | undefined;
  private chatSocket: EventSocket | undefined;
  private progressSocket: EventSocket | undefined;
  private readonly subs: vscode.Disposable[] = [];

  constructor(
    private readonly context: vscode.ExtensionContext,
    private readonly services: Services,
    private readonly runnerAlive: () => boolean | undefined,
    runnerChanged: vscode.Event<unknown>,
  ) {
    this.subs.push(
      services.onDidChange(() => void this.refresh()),
      runnerChanged(() => this.render()),
    );
  }

  static register(
    context: vscode.ExtensionContext,
    services: Services,
    runnerAlive: () => boolean | undefined,
    runnerChanged: vscode.Event<unknown>,
  ): ChatViewProvider {
    const provider = new ChatViewProvider(context, services, runnerAlive, runnerChanged);
    context.subscriptions.push(
      provider,
      vscode.window.registerWebviewViewProvider(ChatViewProvider.viewType, provider, {
        webviewOptions: { retainContextWhenHidden: true },
      }),
      vscode.commands.registerCommand('escurel.chat.askAboutRecord', (pageId, title) =>
        provider.askAboutRecord(pageId, title),
      ),
      vscode.commands.registerCommand('escurel.chat.addToFocus', (arg) => provider.addToFocus(arg)),
      vscode.commands.registerCommand('escurel.chat.new', async () => {
        provider.newChat();
        await vscode.commands.executeCommand(`${ChatViewProvider.viewType}.focus`);
      }),
    );
    return provider;
  }

  resolveWebviewView(view: vscode.WebviewView): void {
    this.view = view;
    view.webview.options = webviewOptions(this.context.extensionUri);
    view.webview.html = webviewHtml(
      view.webview,
      this.context.extensionUri,
      'chat',
      'escurel-chat',
    );
    const sub = view.webview.onDidReceiveMessage((m: unknown) => void this.handleMessage(m));
    this.viewSub?.dispose();
    this.viewSub = sub;
    view.onDidDispose(() => {
      sub.dispose();
      if (this.view === view) this.view = undefined;
    });
  }

  /** What the view shows (for the integration suite, which cannot read a webview). */
  state(): ChatViewState {
    return {
      status: this.status,
      ...(this.message ? { message: this.message } : {}),
      recent: this.recent.map((c, i) => ({
        key: `recent:${i}`,
        title: c.title,
        when: c.startedAt,
      })),
      current: this.current && {
        key: this.current.record
          ? 'record'
          : `recent:${this.recent.findIndex((c) => c.pageId === this.current?.pageId)}`,
        title: this.current.record ? `About ${this.current.title}` : this.current.title,
      },
      rows: this.rows(),
      focus: this.focus.map((f, i) => ({ key: `focus:${i}`, title: f.title })),
      canSend: this.status === 'ready' && !this.sending,
    };
  }

  /**
   * A message from the view. Public so the integration suite can drive the host without a click in
   * a webview it cannot reach. Returns whether the host acted on it.
   */
  async handleMessage(raw: unknown): Promise<boolean> {
    const m = raw as ChatWebviewToHost | undefined;
    if (!m || typeof m !== 'object') return false;
    switch (m.type) {
      case 'ready':
        await this.refresh();
        return true;
      case 'new-chat':
        this.newChat();
        return true;
      case 'open-conversation': {
        const conv = this.recentByKey(m.key);
        if (!conv) return false;
        await this.open(conv);
        return true;
      }
      case 'send':
        if (typeof m.text !== 'string') return false;
        return this.send(m.text);
      case 'open-wikilink': {
        const target = typeof m.wikilink === 'string' ? m.wikilink.replace(/^\[\[|\]\]$/g, '') : '';
        if (!this.citedTargets().has(target)) {
          log().warn('chat: refused a link no answer on show cites');
          return false;
        }
        await vscode.commands.executeCommand('escurel.resolve', `[[${target}]]`);
        return true;
      }
      case 'show-run': {
        const row = this.rows().find((r) => r.key === m.key);
        if (row?.kind !== 'assistant' || !row.runId) return false;
        await vscode.commands.executeCommand('escurel.openRun', row.runId);
        return true;
      }
      case 'review-draft': {
        const row = this.rows().find((r) => r.key === m.key);
        const draftId =
          row?.kind === 'assistant' && Number.isInteger(m.index) ? row.drafts[m.index] : undefined;
        if (!draftId) return false;
        await vscode.commands.executeCommand('escurel.openReview', { draftId });
        return true;
      }
      case 'remove-focus': {
        const n = typeof m.key === 'string' ? /^focus:(\d+)$/.exec(m.key)?.[1] : undefined;
        if (n === undefined || !this.focus[Number(n)]) return false;
        this.focus = this.focus.filter((_, i) => i !== Number(n));
        this.render();
        return true;
      }
      default:
        return false;
    }
  }

  /** "Ask about this record": the record's own page chat, with the record as its context. */
  async askAboutRecord(pageId: unknown, title: unknown): Promise<void> {
    if (typeof pageId !== 'string' || !pageId.startsWith('markdown/')) return;
    if (!this.subject) this.subject = await this.services.subject();
    if (!this.subject) return;
    const name = typeof title === 'string' && title ? cleanText(title, 80) : pageId;
    await this.open({ pageId, title: name, startedAt: '' }, true);
    await vscode.commands.executeCommand(`${ChatViewProvider.viewType}.focus`);
  }

  /** "Add to chat focus" from the Knowledge tree: pin a page as context (at most 10). */
  addToFocus(arg: unknown): void {
    const pageId =
      typeof arg === 'string' ? arg : (arg as { pageId?: unknown } | undefined)?.pageId;
    if (typeof pageId !== 'string' || !pageId.startsWith('markdown/')) return;
    if (this.focus.some((f) => f.pageId === pageId) || this.focus.length >= MAX_FOCUS) return;
    const title = pageId.split('/').pop()?.replace(/\.md$/, '') ?? pageId;
    this.focus = [...this.focus, { pageId, title: cleanText(title, 60) }];
    this.render();
    void vscode.commands.executeCommand(`${ChatViewProvider.viewType}.focus`);
  }

  newChat(): void {
    this.closeSockets();
    this.current = undefined;
    this.messages = [];
    this.progress = undefined;
    this.render();
  }

  dispose(): void {
    this.closeSockets();
    this.viewSub?.dispose();
    for (const s of this.subs) s.dispose();
  }

  private recentByKey(key: unknown): Conversation | undefined {
    if (typeof key !== 'string') return undefined;
    const n = /^recent:(\d+)$/.exec(key)?.[1];
    return n === undefined ? undefined : this.recent[Number(n)];
  }

  private rows(): ChatRow[] {
    return foldChat(this.messages, { runnerAlive: this.runnerAlive(), progress: this.progress });
  }

  /** The link targets the answers on show cite: the only links the view may open. */
  private citedTargets(): Set<string> {
    return new Set(
      this.messages.filter((m) => m.role === 'assistant').flatMap((m) => wikilinksIn(m.content)),
    );
  }

  private async refresh(): Promise<void> {
    this.subject = await this.services.subject();
    if (!this.subject) {
      this.status = 'signed-out';
      this.message = 'Sign in to chat: a conversation is private to the person who starts it.';
      this.recent = [];
      this.newChat();
      return;
    }
    try {
      const skills = new Set((await this.services.client.listSkills()).map((s) => s.id));
      if (!skills.has(CHAT_SKILL) || !skills.has('conversation')) {
        this.status = 'unavailable';
        this.message =
          'Chat is not set up in this workspace. An admin can install the chat skills (examples/chat/seed).';
        this.render();
        return;
      }
      this.recent = await recentConversations(this.services.client);
      this.status = 'ready';
      this.message = undefined;
    } catch (e) {
      this.status = 'error';
      this.message = `Could not load your conversations: ${describeError(e)}`;
    }
    if (this.current) await this.loadMessages();
    this.render();
  }

  private async open(conv: Conversation, record = false): Promise<void> {
    if (!this.subject) return;
    this.closeSockets();
    this.current = {
      ...conv,
      group: chatGroupId(this.subject, conv.pageId),
      ...(record ? { record: true as const } : {}),
    };
    this.messages = [];
    this.progress = undefined;
    this.render();
    this.chatSocket = new EventSocket({
      gatewayUrl: this.services.gatewayUrl,
      tokens: this.services.auth.refresher,
      filters: { chat_group_id: this.current.group },
      onEvent: () => undefined,
      onChatMessage: (msg) => this.add(msg),
      onWarning: () => void this.loadMessages().then(() => this.render()),
      onError: (e) => log().warn(`chat: live updates stopped: ${e.message}`),
      // A (re)connect has no replay: read what was missed.
      onConnect: () => void this.loadMessages().then(() => this.render()),
    });
    this.chatSocket.connect();
  }

  private async loadMessages(): Promise<void> {
    const current = this.current;
    if (!current) return;
    try {
      const page = await this.services.client.listMessages({
        chat_group_id: current.group,
        direction: 'desc',
        limit: HISTORY_LIMIT,
      });
      if (this.current?.group !== current.group) return;
      const known = new Map(this.messages.map((m) => [m.msg_id, m]));
      for (const m of page.messages) known.set(m.msg_id, m);
      this.messages = [...known.values()];
    } catch (e) {
      log().warn(`chat: could not read the conversation: ${describeError(e)}`);
    }
  }

  private add(msg: ChatMessage): void {
    if (!this.current || msg.chat_group_id !== this.current.group) return;
    this.messages = [...this.messages.filter((m) => m.msg_id !== msg.msg_id), msg];
    if (msg.role === 'assistant') {
      this.progress = undefined;
      this.progressSocket?.close();
      this.progressSocket = undefined;
    }
    this.render();
  }

  private async send(raw: string): Promise<boolean> {
    const text = raw.trim();
    if (!text || text.length > MAX_QUESTION || this.status !== 'ready' || this.sending)
      return false;
    const subject = this.subject;
    if (!subject) return false;
    this.sending = true;
    this.render();
    try {
      if (!this.current) {
        const conv = await createConversation(this.services.client, subject, text);
        this.recent = [conv, ...this.recent];
        await this.open(conv);
      }
      const current = this.current!;
      const msgId = randomUUID();
      // Shown at once; the socket's copy of the same msg_id replaces it.
      this.add({
        chat_group_id: current.group,
        msg_id: msgId,
        ts: new Date().toISOString(),
        role: 'user',
        content: text,
        embedded: false,
        author: subject,
      });
      await this.services.client.appendMessage({
        chat_group_id: current.group,
        role: 'user',
        content: text,
        msg_id: msgId,
        embed: true,
      });
      const event = await this.services.client.captureEvent({
        label_skill: CHAT_SKILL,
        source: 'vscode',
        instance_page_id: current.pageId,
        title: 'chat turn',
        body: '',
        provenance: {
          chat_group_id: current.group,
          user_msg_id: msgId,
          ...(this.focus.length ? { focus: this.focus.map((f) => f.pageId) } : {}),
        },
      });
      this.watchProgress(event.event_id);
      return true;
    } catch (e) {
      void vscode.window.showErrorMessage(`Could not send your question: ${describeError(e)}`);
      return false;
    } finally {
      this.sending = false;
      this.render();
    }
  }

  /** The question's run reports its steps as `run-progress` events in the question's lineage. */
  private watchProgress(rootEventId: string): void {
    this.progressSocket?.close();
    this.progressSocket = new EventSocket({
      gatewayUrl: this.services.gatewayUrl,
      tokens: this.services.auth.refresher,
      filters: { root_event_id: rootEventId },
      onEvent: (e: Event) => {
        if (e.title !== 'run-progress') return;
        this.progress = progressStep(e.body);
        this.render();
      },
      onWarning: () => undefined,
      onError: () => undefined,
    });
    this.progressSocket.connect();
  }

  private closeSockets(): void {
    this.chatSocket?.close();
    this.progressSocket?.close();
    this.chatSocket = undefined;
    this.progressSocket = undefined;
  }

  private render(): void {
    const view = this.view;
    if (!view) return;
    const message: ChatHostToWebview = { type: 'chat-state', state: this.state() };
    safePost(view, message);
  }
}

/** The step a `run-progress` body says the run is on: `current`, else the in-progress step. */
export function progressStep(body: unknown): string | undefined {
  let parsed: unknown = body;
  if (typeof body === 'string') {
    try {
      parsed = JSON.parse(body);
    } catch {
      return undefined;
    }
  }
  const o = parsed as { current?: unknown; plan?: unknown } | null;
  if (typeof o?.current === 'string' && o.current) return cleanText(o.current, 120);
  const step = Array.isArray(o?.plan)
    ? (o.plan as { step?: unknown; status?: unknown }[]).find((s) => s.status === 'in_progress')
    : undefined;
  return typeof step?.step === 'string' ? cleanText(step.step, 120) : undefined;
}
