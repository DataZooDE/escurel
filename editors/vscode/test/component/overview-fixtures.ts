import type { OverviewView } from '../../src/shared/protocol';

/** A morning on the demo tenant: one decision waiting, one agent running, one failure, orders and suppliers open. */
export const morning: OverviewView = {
  focusOn: true,
  updatedAt: '2026-10-06T08:15:00.000Z',
  tiles: [
    {
      id: 'decisions',
      title: 'Decisions waiting',
      headline: '2 waiting for you',
      tone: 'attention',
      empty: 'Nothing needs your decision right now.',
      items: [
        {
          key: 'decisions:0',
          label: 'order-4500131 — Proposed changes',
          detail: '2 changes · supplier-risk agent · 3 m',
          tone: 'attention',
        },
        {
          key: 'decisions:1',
          label: 'order-4500140 — Plan ready',
          detail: 'supplier-risk agent · 1 h',
          tone: 'attention',
        },
      ],
    },
    {
      id: 'agents',
      title: 'Agent activity',
      headline: 'Agents are running · last seen just now',
      tone: 'ok',
      empty: 'No agent is running right now.',
      items: [
        {
          key: 'agents:0',
          label: 'Running · supplier-risk · order-4500152',
          detail: '12 s',
          tone: 'neutral',
        },
      ],
    },
    {
      id: 'attention',
      title: 'Needs attention',
      headline: '1 needs a look',
      tone: 'attention',
      empty: 'No run has failed.',
      items: [
        {
          key: 'attention:0',
          label: 'Failed · delivery-check · order-4500118',
          detail: 'the carrier portal did not answer · 2 h',
          tone: 'attention',
        },
      ],
    },
    {
      id: 'open',
      title: 'Open items',
      headline: '3 kinds of work',
      tone: 'neutral',
      empty: 'There are no records yet.',
      more: 1,
      items: [
        { key: 'open:0', label: 'Customer order', detail: '50+ records', tone: 'neutral' },
        { key: 'open:1', label: 'Supplier', detail: '12 records', tone: 'neutral' },
        { key: 'open:2', label: 'Delivery confirmation', detail: '2 records', tone: 'neutral' },
      ],
    },
    {
      id: 'recent',
      title: 'Recently finished',
      headline: 'Last 24 h · 8 runs',
      tone: 'neutral',
      empty: 'No agent has finished anything yet.',
      items: [
        {
          key: 'recent:0',
          label: 'Done · supplier-risk · order-4500123',
          detail: '6 s · 40 m',
          tone: 'ok',
        },
      ],
    },
  ],
};

/** All quiet: every tile at its empty state. */
export const quiet: OverviewView = {
  focusOn: false,
  updatedAt: '2026-10-06T08:15:00.000Z',
  tiles: morning.tiles.map(({ more: _more, ...t }) => ({
    ...t,
    items: [],
    tone: 'ok' as const,
    headline: 'All clear',
  })),
};
