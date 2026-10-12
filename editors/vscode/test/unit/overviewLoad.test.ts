import { describe, expect, it } from 'vitest';
import type { Skill } from '../../src/client';
import type { EscurelClient } from '../../src/client';
import { countDelayed, OPEN_SKILLS_MAX, pickOpenSkills } from '../../src/overview/load';

const skill = (id: string, over: Partial<Skill> = {}): Skill =>
  ({ id, is_event_typed: false, ...over }) as unknown as Skill;

describe('which skills the "Open items" tile counts', () => {
  it('counts records and processes, not events, reports, helpers or the engine’s own skills', () => {
    const picked = pickOpenSkills([
      skill('customer-order', { role: 'record' }),
      skill('supplier-risk-analysis', { role: 'process' }),
      skill('meeting', { is_event_typed: true }),
      skill('risk-report', { role: 'report' }),
      skill('helper-thing', { role: 'helper' }),
      skill('escurel:run'),
      skill('plain-skill'),
    ]);
    expect(picked.map((s) => s.id)).toEqual([
      'customer-order',
      'supplier-risk-analysis',
      'plain-skill',
    ]);
  });

  it('is bounded: a tenant with hundreds of skills costs a handful of reads', () => {
    const many = Array.from({ length: 200 }, (_, i) => skill(`s${i}`, { role: 'record' }));
    expect(pickOpenSkills(many)).toHaveLength(OPEN_SKILLS_MAX);
  });

  it('names a skill by its title when it has one, else by its id in words', () => {
    const [a, b] = pickOpenSkills([
      skill('customer-order', { title: 'Customer orders' }),
      skill('supplier_check'),
    ]);
    expect([a!.title, b!.title]).toEqual(['Customer orders', 'Supplier check']);
  });
});

type Row = { page_id: string; frontmatter?: Record<string, unknown> };
/** A client that serves `listInstancesPage` from a table; counts the reads it was asked for. */
function clientOf(pages: (req: Record<string, unknown>) => { instances: Row[]; next_cursor?: string | null }) {
  const reads: Record<string, unknown>[] = [];
  const client = {
    listInstancesPage: async (req: Record<string, unknown>) => {
      reads.push(req);
      return pages(req);
    },
  } as unknown as EscurelClient;
  return { client, reads };
}
const declared = (over: Record<string, unknown> = {}, extra: Partial<Skill> = {}): Skill =>
  skill('shipment', {
    fields: [{ name: 'status' }, { name: 'days_late' }],
    overview: { tile: 'Delayed shipments', status_field: 'status', delayed_when: ['late', 'delayed'], sort_field: 'days_late', ...over },
    ...extra,
  } as Partial<Skill>);

describe('the skill-declared "delayed" tile', () => {
  it('reads each delayed value once, counts distinct instances, orders by the sort field, keeps three lines', async () => {
    const { client, reads } = clientOf((req) => ({
      instances:
        req.frontmatter_value === 'late'
          ? [1, 2, 3].map((n) => ({ page_id: `markdown/instances/shipment/s${n}.md`, frontmatter: { title: `Order ${n}`, days_late: n * 2 } }))
          : [{ page_id: 'markdown/instances/shipment/s3.md', frontmatter: { title: 'Order 3', days_late: 6 } },
             { page_id: 'markdown/instances/shipment/s4.md', frontmatter: { title: 'Order 4', days_late: 1 } }],
    }));
    const [tile] = await countDelayed(client, [declared()]);
    expect(reads.map((r) => [r.frontmatter_key, r.frontmatter_value])).toEqual([['status', 'late'], ['status', 'delayed']]);
    expect(tile!.count).toBe(4);
    expect(tile!.instances.map((i) => i.title)).toEqual(['Order 3', 'Order 2', 'Order 1']);
    expect(tile!.instances[0]!.sort).toBe('6');
  });

  it('is bounded to five pages in all and says so when it stopped early', async () => {
    let n = 0;
    const { client, reads } = clientOf(() => ({
      instances: [{ page_id: `markdown/instances/shipment/p${n++}.md`, frontmatter: { title: 't' } }],
      next_cursor: 'more',
    }));
    const [tile] = await countDelayed(client, [declared()]);
    expect(reads).toHaveLength(5);
    expect(tile!.truncated).toBe(true);
    expect(tile!.count).toBe(5);
  });

  it('omits the tile, with no read, when a rows skill cannot filter on the status field', async () => {
    const { client, reads } = clientOf(() => ({ instances: [] }));
    const rows = declared({}, { backend: { kind: 'sql_view', instances: 'rows', filterable: [{ field: 'sold_to', column: 'kunnr' }] } } as Partial<Skill>);
    expect(await countDelayed(client, [rows])).toEqual([]);
    expect(reads).toHaveLength(0);
  });

  it('reads a rows skill whose status field is filterable', async () => {
    const { client, reads } = clientOf(() => ({ instances: [] }));
    const rows = declared({}, { fields: [], backend: { kind: 'sql_view', instances: 'rows', filterable: [{ field: 'status', column: 'st' }] } } as Partial<Skill>);
    const [tile] = await countDelayed(client, [rows]);
    expect(tile!.count).toBe(0);
    expect(reads.length).toBeGreaterThan(0);
  });

  it('ignores malformed declarations and a failing read costs only that tile', async () => {
    const { client } = clientOf(() => { throw new Error('boom'); });
    const bad = [
      declared({ tile: 'x'.repeat(61) }),
      declared({ delayed_when: [] }),
      declared({ status_field: 'nope' }),
    ];
    expect(await countDelayed(client, bad)).toEqual([]);
    expect(await countDelayed(client, [declared()])).toEqual([]);
  });

  it('puts untrusted instance text through the shared cleaner', async () => {
    const { client } = clientOf(() => ({ instances: [{ page_id: 'markdown/instances/shipment/x.md', frontmatter: { title: 'evil\u202Egnp.exe', days_late: '3\u200B' } }] }));
    const [tile] = await countDelayed(client, [declared()]);
    expect(tile!.instances[0]!.title).not.toMatch(/\u202E/);
    expect(tile!.instances[0]!.sort).not.toMatch(/\u200B/);
  });
});
