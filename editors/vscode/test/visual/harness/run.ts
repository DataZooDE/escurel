import '../../../webview/run/main';
import { runControls } from '../../../src/runs/controls';
import type { EscurelRunDetail } from '../../../webview/run/run-detail';
import { recordedRunView } from '../../component/run-fixtures';

const el = document.querySelector('escurel-run-detail') as EscurelRunDetail;

// `?state=` shows the run in a state that offers controls, as a NON-admin sees it: Requeue is
// there but deactivated, with its reason. Without it, the recorded run as it was.
const state = new URLSearchParams(location.search).get('state');
el.view = state
  ? {
      ...recordedRunView,
      status: state,
      tone: state === 'dead_letter' || state === 'failed' ? 'failed' : 'run',
      skill: 'supplier-risk',
      controls: runControls(state, 'not-admin'),
    }
  : recordedRunView;
