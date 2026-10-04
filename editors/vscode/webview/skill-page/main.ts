import type { SkillPageToHost, SkillPageToWebview } from '../../src/shared/skillPage';
import { vscodeApi } from '../shared/vscode-api';
import './skill-page';
import type { EscurelSkillPage } from './skill-page';

export { EscurelSkillPage } from './skill-page';

// Inside VS Code: bind the one component to the host's messages. Outside (tests, the visual harness)
// the component is driven directly.
const api = vscodeApi();
if (api) {
  const el = (document.querySelector('escurel-skill-page') ??
    document.body.appendChild(document.createElement('escurel-skill-page'))) as EscurelSkillPage;
  el.addEventListener('escurel-message', (e) =>
    api.postMessage((e as CustomEvent<SkillPageToHost>).detail),
  );
  window.addEventListener('message', (e: MessageEvent<SkillPageToWebview>) => {
    const msg = e.data;
    if (msg.type === 'skill') {
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
