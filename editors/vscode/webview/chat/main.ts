import type { ChatHostToWebview, ChatWebviewToHost } from '../../src/shared/protocol';
import { vscodeApi } from '../shared/vscode-api';
import './chat';
import type { EscurelChat } from './chat';

export interface ChatApi {
  postMessage(message: ChatWebviewToHost): void;
}

/** Connects the Chat view to the host over typed postMessage. */
export function connectChatWebview(api: ChatApi, el: EscurelChat): () => void {
  const forward = (event: Event) =>
    api.postMessage((event as CustomEvent<ChatWebviewToHost>).detail);
  const receive = (event: MessageEvent<ChatHostToWebview>) => {
    if (event.data?.type === 'chat-state') el.state = event.data.state;
  };
  el.addEventListener('escurel-message', forward);
  window.addEventListener('message', receive);
  api.postMessage({ type: 'ready' });
  return () => {
    el.removeEventListener('escurel-message', forward);
    window.removeEventListener('message', receive);
  };
}

const api = vscodeApi();
if (api) {
  const el = (document.querySelector('escurel-chat') ??
    document.body.appendChild(document.createElement('escurel-chat'))) as EscurelChat;
  connectChatWebview(api as unknown as ChatApi, el);
}
