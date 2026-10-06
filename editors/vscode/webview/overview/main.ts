import type { OverviewHostToWebview, OverviewWebviewToHost } from '../../src/shared/protocol';
import { vscodeApi } from '../shared/vscode-api';
import './overview';
import type { EscurelOverview } from './overview';

export { EscurelOverview } from './overview';

// Inside VS Code: bind the one component to the host's messages. Outside (tests, the visual harness)
// the component is driven directly.
const api = vscodeApi();
if (api) {
  const el = (document.querySelector('escurel-overview') ??
    document.body.appendChild(document.createElement('escurel-overview'))) as EscurelOverview;
  el.addEventListener('escurel-message', (e) =>
    api.postMessage((e as CustomEvent<OverviewWebviewToHost>).detail),
  );
  window.addEventListener('message', (e: MessageEvent<OverviewHostToWebview>) => {
    const msg = e.data;
    if (msg.type === 'overview') {
      el.view = msg.view;
      el.error = undefined;
    } else if (msg.type === 'overview-error') {
      el.error = msg.message;
    } else if (msg.type === 'overview-loading') {
      // Keep what is on screen while it reloads: a board that blanks every 20 s flickers.
      if (!el.view) el.error = undefined;
    }
  });
  api.postMessage({ type: 'ready' });
}
