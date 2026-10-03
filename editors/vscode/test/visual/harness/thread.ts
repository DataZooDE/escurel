import '../../../webview/thread/main';
import { runControls } from '../../../src/runs/controls';
import type { InspectorView } from '../../../src/shared/protocol';
import type { EscurelThreadCanvas } from '../../../webview/thread/thread-canvas';
import {
  branchingFocus,
  branchingLayout,
  branchingThreadView,
  recordedDetails,
  recordedFocus,
  recordedLayout,
  recordedThreadView,
} from '../../component/thread-fixtures';

const el = document.querySelector('escurel-thread-canvas') as EscurelThreadCanvas;
// `?scenario=branches`: the hand-built thread with work waiting on a person and a second lane.
const branches = new URLSearchParams(location.search).get('scenario') === 'branches';
el.view = branches ? branchingThreadView : recordedThreadView;
el.layout = branches ? branchingLayout : recordedLayout;
el.focus = branches ? branchingFocus : recordedFocus;
el.details = recordedDetails;

// Fit, so the baseline shows the WHOLE thread. The first baselines were taken at 100% in a
// 900px window and silently clipped the cascade hop off the right edge: a screenshot of
// half a thread passes as happily as one of all of it.
void el.updateComplete.then(() => {
  el.fit();
});

// `?select=run` / `?select=draft` selects a node that offers actions, so the inspector's buttons are
// in the baseline: the run as a NON-admin sees a dead letter (Requeue deactivated, with its reason),
// and the instance a draft proposes a change to (the skill's actions as Skill split buttons).
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
    el.details = {
      ...recordedDetails,
      [node.id]: {
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
    void el.updateComplete.then(() => el.selectNode(node.id));
  }
}
