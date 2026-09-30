import type { RunHostToWebview, RunWebviewToHost } from '../../src/shared/protocol';
import { vscodeApi } from '../shared/vscode-api';
import './run-detail';
import type { EscurelRunDetail } from './run-detail';

export { EscurelRunDetail } from './run-detail';

interface RunApi {
  postMessage(message: RunWebviewToHost): void;
}

export function connectRunWebview(api: RunApi, el: EscurelRunDetail): () => void {
  const forward = (event: Event) =>
    api.postMessage((event as CustomEvent<RunWebviewToHost>).detail);
  const receive = (event: MessageEvent<RunHostToWebview>) => {
    const message = event.data;
    if (message.type === 'run') {
      el.view = message.view;
      el.error = undefined;
    } else if (message.type === 'run-loading') {
      el.view = undefined;
      el.error = undefined;
    } else if (message.type === 'run-error') {
      el.error = { message: message.message, canReconnect: message.canReconnect };
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
  const el = (document.querySelector('escurel-run-detail') ??
    document.body.appendChild(document.createElement('escurel-run-detail'))) as EscurelRunDetail;
  connectRunWebview(api as unknown as RunApi, el);
}
