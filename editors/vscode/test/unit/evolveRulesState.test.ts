/* eslint-disable @typescript-eslint/no-explicit-any -- JSON fixtures captured from the real service */
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import {
  STATE_INSTANCE_KEYS,
  instanceOnly,
  readStateInstance,
  stateStartRequest,
} from '../../src/evolve/stateInstance';

const H = (c: string) => c.repeat(64);
const fx = (name: string): any =>
  JSON.parse(readFileSync(join(__dirname, 'fixtures/evolve-rules', name + '.json'), 'utf8'));

const p0 = { pilot: 'p0', capacity: 10, items_csv: '6,5,4,7,2,3,8,1,9,4,6,2,5,3,7,1' };
const p5 = fx('instance_p5');
const p7 = fx('instance_p7');
const INSTANCES: Record<string, Record<string, unknown>> = { p0, p5, p7 };

describe('the instance a person points at (client-side allow-list)', () => {
  it('names the keys each pilot reads, and nothing else', () => {
    expect(STATE_INSTANCE_KEYS).toEqual({
      p0: ['capacity', 'items_csv'],
      p5: ['shelf_capacity', 'products_csv', 'subs_csv'],
      p7: ['max_holdings', 'max_position_size', 'assets_csv', 'covariance_csv'],
    });
  });

  it.each(Object.keys(INSTANCES))('accepts the real %s instance unchanged', (pilot) => {
    expect(readStateInstance(INSTANCES[pilot])).toEqual(INSTANCES[pilot]);
    expect(instanceOnly(readStateInstance(INSTANCES[pilot]))).toEqual(INSTANCES[pilot]);
  });

  it('refuses an unknown key before anything is sent, naming it', () => {
    for (const pilot of Object.keys(INSTANCES))
      expect(() => readStateInstance({ ...INSTANCES[pilot], synthetic_brain: 'x' })).toThrow(
        /synthetic_brain/,
      );
    // a key of another pilot is unknown here too
    expect(() => readStateInstance({ ...p0, subs_csv: '1:2:0.3' })).toThrow(/subs_csv/);
    // a typo of the substitution table is the real-world case
    expect(() => readStateInstance({ ...p5, substitution_csv: '1:2:0.3' })).toThrow(
      /substitution_csv/,
    );
    // server-owned keys are never the client's
    for (const key of ['rule_set', 'rule_set_p_max', 'approved_rule_set_sha256', 'seed_sql'])
      expect(() => readStateInstance({ ...p0, [key]: 'x' })).toThrow(new RegExp(key));
  });

  it('refuses a missing or foreign pilot, and a missing or mistyped instance field', () => {
    expect(() => readStateInstance({ capacity: 10, items_csv: '1' })).toThrow(/pilot/);
    expect(() => readStateInstance({ ...p0, pilot: 'p1_decision' })).toThrow(/p0, p5 or p7/);
    expect(() => readStateInstance({ pilot: 'p0', capacity: 10 })).toThrow(/items_csv/);
    expect(() => readStateInstance({ ...p0, capacity: '10' })).toThrow(/capacity/);
    expect(() => readStateInstance({ ...p0, capacity: -1 })).toThrow(/capacity/);
    expect(() => readStateInstance({ ...p7, max_position_size: 'half' })).toThrow(
      /max_position_size/,
    );
    expect(() => readStateInstance({ ...p0, items_csv: '' })).toThrow(/items_csv/);
    expect(() => readStateInstance([p0])).toThrow();
  });

  it('accepts the start keys, only in their shape', () => {
    const ok = readStateInstance({
      ...p0,
      experiment: 'e-1',
      brain: 'llm',
      max_generations: 0,
      budget: { max_evaluated: 1 },
    });
    expect(ok.max_generations).toBe(0);
    expect(() => readStateInstance({ ...p0, brain: 'scripted' })).toThrow(/brain/);
    expect(() => readStateInstance({ ...p0, max_generations: -1 })).toThrow(/max_generations/);
    expect(() => readStateInstance({ ...p0, max_generations: 1.5 })).toThrow(/max_generations/);
    expect(() => readStateInstance({ ...p0, budget: 3 })).toThrow(/budget/);
    expect(() => readStateInstance({ ...p0, experiment: 'bad id!' })).toThrow(/experiment/);
  });

  it('drops the start keys from what is proposed', () => {
    const spec = readStateInstance({ ...p0, experiment: 'e-1', max_generations: 3 });
    expect(instanceOnly(spec)).toEqual(p0);
  });
});

describe('the start request of a state experiment', () => {
  const start = { experiment: 'exp-1', max_generations: 0, budget: { max_evaluated: 1 } };

  it.each(Object.keys(INSTANCES))(
    '%s: is exactly the instance, the start keys, brain llm and the hash',
    (pilot) => {
      const spec = readStateInstance({ ...INSTANCES[pilot], ...start });
      expect(stateStartRequest(spec, H('9'))).toEqual({
        ...INSTANCES[pilot],
        ...start,
        brain: 'llm',
        approved_rule_set_sha256: H('9'),
      });
    },
  );

  it('takes max_generations from the file and refuses a file without it', () => {
    const spec = readStateInstance({ ...p0, experiment: 'exp-1' });
    expect(() => stateStartRequest(spec, H('9'))).toThrow(/max_generations/);
  });

  it('needs an experiment id of its own', () => {
    const spec = readStateInstance({ ...p0, max_generations: 0 });
    expect(() => stateStartRequest(spec, H('9'))).toThrow(/experiment/);
  });

  it('refuses a malformed hash', () => {
    const spec = readStateInstance({ ...p0, ...start });
    expect(() => stateStartRequest(spec, 'nope')).toThrow(/64-character/);
  });
});
