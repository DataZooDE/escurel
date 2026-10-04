import { describe, expect, it } from 'vitest';
import type { Event } from '../../src/client';
import { normalizeV2TrainingSpec, preparedV2Draft, v2TrainingStarter } from '../../src/evolve/problemImport';
import {
  parsePreparationReceipt, preparationEvent, preparationReceiptId,
  trainingSourcePage, trainingSourcePayload,
} from '../../src/evolve/sourceImport';

describe('private V2 training source preparation', () => {
  it('opens an incomplete source-only policy draft and rejects it for import', () => {
    const source = trainingSourcePayload({ ...v2TrainingStarter, skus: [
      { ...v2TrainingStarter.skus[0], sku_id: 42 },
    ] });
    const draft = preparedV2Draft(source, source, 'src_42', 'b'.repeat(64));
    expect(draft.unit_order_costs).toEqual({});
    expect(draft.seed_sql).toBe('REPLACE_WITH_SEED_SQL');
    expect(() => normalizeV2TrainingSpec(draft, 'private-holdout')).toThrow(/Fill seed_sql/);
  });
  it('extracts only training inputs from a full spec and binds the event to stored bytes', () => {
    const page = trainingSourcePage({ id: 'august-training', owner: 'alice', value: v2TrainingStarter });
    expect(page.pageId).toBe('markdown/instances/evolve_training_source/august-training.md');
    expect(page.payload).not.toHaveProperty('seed_sql');
    expect(page.payload).not.toHaveProperty('source_sha256');
    expect(page.content).toContain('skill: evolve_training_source');
    expect(page.content).toContain('```json');
    const event = preparationEvent(page.pageId, 'a'.repeat(64));
    expect(event.label_skill).toBe('evolve_prepare_source');
    expect((event.provenance as { manual: Record<string, unknown> }).manual.expected_page_sha256)
      .toBe('a'.repeat(64));
    expect(event.body).not.toContain('demand');
  });

  it('rejects a stockout-censored demand declaration and unrelated receipts', () => {
    expect(() => trainingSourcePayload({ ...v2TrainingStarter, demand_observation: 'sales_only' }))
      .toThrow(/true demand/);
    expect(() => trainingSourcePayload({
      ...trainingSourcePayload(v2TrainingStarter), holdout_demand: [1, 2],
    })).toThrow(/Unsupported source key holdout_demand/);
    const eventId = 'prepared-click';
    const receipt = {
      event_id: preparationReceiptId(eventId), kind: 'system', source: 'anofox-evolve',
      label_skill: 'evolve:training-source', root_event_id: eventId,
      body: JSON.stringify({ prepared: true, source_page_sha256: 'a'.repeat(64),
        training_source_id: 'src_test', normalized_sha256: 'b'.repeat(64) }),
    } as Event;
    expect(parsePreparationReceipt(receipt, eventId, 'a'.repeat(64))?.training_source_id)
      .toBe('src_test');
    expect(parsePreparationReceipt(receipt, 'other-click', 'a'.repeat(64))).toBeUndefined();
    expect(parsePreparationReceipt(receipt, eventId, 'c'.repeat(64))).toBeUndefined();
  });
});
