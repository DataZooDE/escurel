import type { SkillPageModel } from '../../src/shared/skillPage';

// Ages are shown relative to now, so the fixture is anchored to now: the pixels stay the same on any day.
const ago = (hours: number) => new Date(Date.now() - hours * 3_600_000 - 60_000).toISOString();

/** What a procurement user sees for the `customer-order` skill: a record type with links and a follow-up. */
export const orderSkillPage: SkillPageModel = {
  id: 'customer-order',
  pageId: 'markdown/skills/customer-order.md',
  title: 'Customer order',
  description: 'A customer order and what is done with it, from credit check to delivery.',
  summary: 'One order per customer purchase.',
  readOnly: false,
  stale: true,
  provenance: [
    'stale',
    'verified 2026-01-10',
    'status reviewed',
    'stale after 90 days',
    '2 sources',
  ],
  facts: [
    { label: 'Role', value: 'record' },
    { label: 'Folder', value: 'sales/orders' },
    { label: 'Tags', value: 'erp, sales' },
    {
      label: 'Data from',
      value: 'SQL table',
      hint: 'Where the records of this skill are stored or read from.',
    },
    { label: 'Agent changes', value: 'wait for your approval' },
  ],
  fields: [
    {
      name: 'customer',
      label: 'Customer',
      required: true,
      detail: 'link to customer',
      description: 'Who placed the order.',
    },
    { name: 'status', label: 'Status', required: false, detail: 'one of open, shipped, closed' },
    { name: 'value_eur', label: 'Value eur', required: false, detail: 'float' },
  ],
  actions: [{ skill: 'credit-check', label: 'Check credit' }],
  instances: {
    items: [
      { pageId: 'markdown/instances/customer-order__order-4500123.md', title: 'Order 4500123' },
      { pageId: 'markdown/instances/customer-order__order-4500124.md', title: 'order-4500124' },
    ],
    more: true,
  },
  runs: [
    {
      rootEventId: '01EVWAIT',
      title: 'Credit limit exceeded for Hoffmann',
      at: ago(4),
      state: 'waiting',
    },
    {
      rootEventId: '01EVDONE',
      runId: '01RUNDONE',
      title: 'Order confirmed',
      at: ago(26),
      state: 'done',
    },
  ],
};
