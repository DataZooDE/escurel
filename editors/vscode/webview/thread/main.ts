import type { ThreadHostToWebview, ThreadWebviewToHost } from '../../src/shared/protocol';
import { vscodeApi } from '../shared/vscode-api';
import './thread-canvas';
import type { EscurelThreadCanvas } from './thread-canvas';

export { EscurelThreadCanvas } from './thread-canvas';

export interface ThreadApi {
  postMessage(message: ThreadWebviewToHost): void;
}

/**
 * Connects the thread webview to the host extension via typed postMessage.
 * Forwards canvas interactions to the host and applies model/error/selection updates.
 */
export function connectThreadWebview(api: ThreadApi, el: EscurelThreadCanvas): () => void {
  const forward = (event: Event) =>
    api.postMessage((event as CustomEvent<ThreadWebviewToHost>).detail);

  const receive = (event: MessageEvent<ThreadHostToWebview>) => {
    const message = event.data;
    if (message.type === 'thread') {
      el.view = message.view;
      el.layout = message.layout;
      el.focus = message.focus;
      el.error = undefined;
    } else if (message.type === 'thread-loading') {
      el.view = undefined;
      el.layout = undefined;
      el.focus = undefined;
      el.error = undefined;
    } else if (message.type === 'thread-error') {
      el.error = { message: message.message, canReconnect: message.canReconnect };
    } else if (message.type === 'thread-select') {
      el.selectNode(message.nodeId);
    }
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
  const el = (document.querySelector('escurel-thread-canvas') ??
    document.body.appendChild(
      document.createElement('escurel-thread-canvas'),
    )) as EscurelThreadCanvas;
  connectThreadWebview(api as unknown as ThreadApi, el);
}
