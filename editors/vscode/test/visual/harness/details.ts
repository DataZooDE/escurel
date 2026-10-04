import '../../../webview/details/main';
import { runControls } from '../../../src/runs/controls';
import type { InspectorView } from '../../../src/shared/protocol';
import type { EscurelDetails } from '../../../webview/details/details';
import { recordedDetails, recordedThreadView } from '../../component/thread-fixtures';

const el = document.querySelector('escurel-details') as EscurelDetails;

// `?select=run` / `?select=draft` shows a node that offers actions, so the buttons are in the
// baseline: the run as a NON-admin sees a dead letter (Requeue deactivated, with its reason), and
// the instance a draft proposes a change to (the skill's actions as Skill split buttons). No
// `select` shows the empty state.
const select = new URLSearchParams(location.search).get('select');
if (select) {
  const kind = select === 'run' ? 'run' : 'draft';
  const node = recordedThreadView.nodes.find((n) => n.kind === kind);
  if (node) {
    const base: InspectorView = recordedDetails[node.id] ?? {
      title: node.title,
      rows: [],
      sideTitle: '',
      side: [],
    };
    el.shown = {
      rootEventId: recordedThreadView.rootEventId,
      nodeId: node.id,
      detail: {
        ...base,
        actions:
          kind === 'run'
            ? { controls: runControls('dead_letter', 'not-admin'), skill: 'supplier-risk' }
            : {
                skills: {
                  pageId: 'markdown/instances/customer-order__order-4500123.md',
                  actions: [
                    { skill: 'supplier-risk', label: 'Reassess risk for 4500123 with an agent' },
                  ],
                },
              },
      },
    };
  }
}
