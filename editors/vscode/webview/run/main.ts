import type { RunHostToWebview, RunWebviewToHost } from '../../src/shared/protocol';
import { vscodeApi } from '../shared/vscode-api';
import './run-detail';
import type { EscurelRunDetail } from './run-detail';

export { EscurelRunDetail } from './run-detail';

const api = vscodeApi() as { postMessage(message: RunWebviewToHost): void } | undefined;
if (api) {
  const el = (document.querySelector('escurel-run-detail') ??
    document.body.appendChild(document.createElement('escurel-run-detail'))) as EscurelRunDetail;
  el.addEventListener('escurel-message', (event) =>
    api.postMessage((event as CustomEvent<RunWebviewToHost>).detail),
  );
  window.addEventListener('message', (event: MessageEvent<RunHostToWebview>) => {
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
  });
  api.postMessage({ type: 'ready' });
}
