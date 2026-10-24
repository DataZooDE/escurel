import type { ReviewPreviewHostToWebview, ReviewPreviewWebviewToHost } from '../../src/shared/protocol';
import { vscodeApi } from '../shared/vscode-api';
import './review-preview';
import type { EscurelReviewPreview } from './review-preview';

export { EscurelReviewPreview } from './review-preview';

// Inside VS Code: bind the one component to the host's messages. Outside (tests) it is driven directly.
const api = vscodeApi();
if (api) {
  const el = (document.querySelector('escurel-review-preview') ??
    document.body.appendChild(document.createElement('escurel-review-preview'))) as EscurelReviewPreview;
  el.addEventListener('escurel-message', (e) =>
    api.postMessage((e as CustomEvent<ReviewPreviewWebviewToHost>).detail),
  );
  window.addEventListener('message', (e: MessageEvent<ReviewPreviewHostToWebview>) => {
    if (e.data?.type === 'review-preview') el.view = e.data.view;
  });
  api.postMessage({ type: 'ready' });
}
