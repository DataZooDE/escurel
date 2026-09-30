import type { ListLineageResponse } from '../../src/client/types';
import type {
  FocusGraph,
  InspectorView,
  ThreadLayout,
  ThreadView,
} from '../../src/shared/protocol';
import { focusGraph, layoutThread } from '../../src/thread/layout';
import { foldLineage, toThreadView } from '../../src/thread/threadModel';
import lineageCascade from '../unit/fixtures/lineage/lineage-cascade.json';

// Build the fixture through the real gateway folding and layout modules.
const folded = foldLineage([lineageCascade as unknown as ListLineageResponse]);
export const recordedThreadView: ThreadView = toThreadView(folded);
export const recordedLayout: ThreadLayout = layoutThread(recordedThreadView, new Set<string>());
export const recordedFocus: FocusGraph = focusGraph(recordedThreadView, recordedLayout);

export const recordedDetails: Record<string, InspectorView> = {
  [recordedThreadView.rootEventId]: {
    title: 'supplier risk',
    rows: [
      { k: 'Skill', v: 'signal' },
      { k: 'Kind', v: 'user' },
    ],
    sideTitle: 'Trigger',
    side: [{ k: 'Status', v: 'processed' }],
  },
  '01M3NHJJGJ1WWAV26F5Z8Y4XKT': {
    title: 'signal run',
    rows: [
      { k: 'Harness', v: 'echo' },
      { k: 'Autonomy', v: 'review' },
    ],
    sideTitle: 'Summary',
    side: [{ k: 'State', v: 'processed' }],
  },
};

// Hand-written open changeset variant to test inline gate buttons (the recorded run has already been promoted).
export const openChangesetThreadView: ThreadView = {
  ...recordedThreadView,
  nodes: recordedThreadView.nodes.map((node) =>
    node.kind === 'changeset'
      ? {
          ...node,
          state: 'open',
          gate: { drafts: 1, changesetId: node.id },
        }
      : node,
  ),
};
export const openChangesetLayout: ThreadLayout = layoutThread(
  openChangesetThreadView,
  new Set<string>(),
);
export const openChangesetFocus: FocusGraph = focusGraph(
  openChangesetThreadView,
  openChangesetLayout,
);

export const gatedThreadView = openChangesetThreadView;
export const gatedLayout = openChangesetLayout;
export const gatedFocus = openChangesetFocus;
