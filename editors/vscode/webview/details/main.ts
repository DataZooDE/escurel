import type { DetailsHostToWebview, DetailsWebviewToHost } from '../../src/shared/protocol';
import { vscodeApi } from '../shared/vscode-api';
import './details';
import type { EscurelDetails } from './details';

export interface DetailsApi {
  postMessage(message: DetailsWebviewToHost): void;
}

/**
 * Connects the details view to the host over typed postMessage: shows what the host sends (the
 * selected node of a thread, or nothing) and forwards the element's messages back.
 */
export function connectDetailsWebview(api: DetailsApi, el: EscurelDetails): () => void {
  const forward = (event: Event) =>
    api.postMessage((event as CustomEvent<DetailsWebviewToHost>).detail);

  const receive = (event: MessageEvent<DetailsHostToWebview>) => {
    const message = event.data;
    if (message?.type === 'details') {
      el.shown = {
        rootEventId: message.rootEventId,
        nodeId: message.nodeId,
        detail: message.detail,
      };
    } else if (message?.type === 'details-empty') {
      el.shown = undefined;
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
  const el = (document.querySelector('escurel-details') ??
    document.body.appendChild(document.createElement('escurel-details'))) as EscurelDetails;
  connectDetailsWebview(api as unknown as DetailsApi, el);
}
