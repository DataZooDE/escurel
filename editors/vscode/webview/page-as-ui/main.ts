import type { HostToWebview, WebviewToHost } from '../../src/shared/protocol';
import { vscodeApi } from '../shared/vscode-api';
import './page-as-ui';
import type { EscurelPageAsUi } from './page-as-ui';

export { EscurelPageAsUi } from './page-as-ui';

// Inside VS Code: bind the one component to the host's messages. Outside
// (tests, the visual harness) the component is driven directly.
const api = vscodeApi();
if (api) {
  const el = (document.querySelector('escurel-page-as-ui') ??
    document.body.appendChild(document.createElement('escurel-page-as-ui'))) as EscurelPageAsUi;
  el.addEventListener('escurel-message', (e) =>
    api.postMessage((e as CustomEvent<WebviewToHost>).detail),
  );
  window.addEventListener('message', (e: MessageEvent<HostToWebview>) => {
    const msg = e.data;
    if (msg.type === 'page') {
      el.model = msg.model;
      el.error = undefined;
    } else if (msg.type === 'error') {
      el.error = msg.message;
    } else if (msg.type === 'loading') {
      el.model = undefined;
      el.error = undefined;
    }
  });
  api.postMessage({ type: 'ready' });
}
