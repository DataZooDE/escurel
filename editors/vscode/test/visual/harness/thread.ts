import '../../../webview/thread/main';
import type { EscurelThreadCanvas } from '../../../webview/thread/thread-canvas';
import {
  recordedDetails,
  recordedFocus,
  recordedLayout,
  recordedThreadView,
} from '../../component/thread-fixtures';

const el = document.querySelector('escurel-thread-canvas') as EscurelThreadCanvas;
el.view = recordedThreadView;
el.layout = recordedLayout;
el.focus = recordedFocus;
el.details = recordedDetails;

// Fit, so the baseline shows the WHOLE thread. The first baselines were taken at 100% in a
// 900px window and silently clipped the cascade hop off the right edge: a screenshot of
// half a thread passes as happily as one of all of it.
void el.updateComplete.then(() => {
  el.fit();
});
