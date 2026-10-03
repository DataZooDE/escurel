import '../../../webview/thread/main';
import type { EscurelThreadCanvas } from '../../../webview/thread/thread-canvas';
import {
  branchingFocus,
  branchingLayout,
  branchingThreadView,
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
// The fixtures' timestamps are fixed; a card's age ("3 min ago") must not depend on today's date.
el.clock = () => new Date('2026-10-03T08:03:06Z');

// `?view=first` keeps the canvas's own first view (100% on the node that needs you, with a
// scrollbar); anything else Fits, so the baseline shows the WHOLE thread. The first baselines were
// taken at 100% in a 900px window and silently clipped the cascade hop off the right edge: a
// screenshot of half a thread passes as happily as one of all of it. Below 70% the cards switch to
// their low-zoom form, so a Fit of a big thread is also the overview baseline.
if (new URLSearchParams(location.search).get('view') !== 'first') {
  void el.updateComplete.then(() => {
    el.fit();
  });
}

// `?zoom=0.4` pins the zoom (top-left), for the lowest-zoom baseline: the words must stay legible
// even when the picture is at 40%.
const zoomParam = new URLSearchParams(location.search).get('zoom');
if (zoomParam) {
  void el.updateComplete.then(() => {
    el.viewport = { x: 10, y: 40, zoom: Number(zoomParam) };
  });
}
