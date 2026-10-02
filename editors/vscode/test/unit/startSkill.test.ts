import { describe, expect, it } from 'vitest';
import { EscurelError } from '../../src/client';
import {
  formatStartError,
  parseStartSkillInput,
  planReadyMessage,
  resolveStartAction,
  skillFromPageId,
  startedMessage,
} from '../../src/start/startSkill';

describe('skillFromPageId', () => {
  it('derives skill from flat instance path', () => {
    expect(skillFromPageId('markdown/instances/customer-order__order-4500123.md')).toBe(
      'customer-order',
    );
    expect(skillFromPageId('instances/supplier-risk__meier.md')).toBe('supplier-risk');
  });

  it('derives skill from nested instance path', () => {
    expect(skillFromPageId('markdown/instances/renewal/c1.md')).toBe('renewal');
    expect(skillFromPageId('instances/renewal/c1.md')).toBe('renewal');
  });

  it('returns undefined for non-instance paths', () => {
    expect(skillFromPageId('markdown/skills/renewal.md')).toBeUndefined();
    expect(skillFromPageId('markdown/other.md')).toBeUndefined();
  });
});

describe('parseStartSkillInput', () => {
  it('parses complete payload from page-as-UI', () => {
    const input = {
      skill: 'renewal',
      pageId: 'markdown/instances/renewal/c1.md',
      mode: 'background' as const,
    };
    const parsed = parseStartSkillInput(input);
    expect(parsed).toEqual({
      skill: 'renewal',
      pageId: 'markdown/instances/renewal/c1.md',
      mode: 'background',
    });
  });

  it('parses a Knowledge tree skill node', () => {
    const input = {
      kind: 'skill' as const,
      skill: { id: 'supplier-risk' },
    };
    const parsed = parseStartSkillInput(input);
    expect(parsed).toEqual({
      skill: 'supplier-risk',
      pageId: undefined,
      mode: undefined,
    });
  });

  it('parses a Knowledge tree instance node and derives skill from path', () => {
    const input = {
      kind: 'instance' as const,
      pageId: 'markdown/instances/customer-order__order-4500123.md',
    };
    const parsed = parseStartSkillInput(input);
    expect(parsed).toEqual({
      skill: 'customer-order',
      pageId: 'markdown/instances/customer-order__order-4500123.md',
      mode: undefined,
    });
  });

  it('parses a Knowledge tree instance node that already carries skill', () => {
    const input = {
      kind: 'instance' as const,
      pageId: 'markdown/instances/customer-order__order-4500123.md',
      skill: 'customer-order',
    };
    const parsed = parseStartSkillInput(input);
    expect(parsed).toEqual({
      skill: 'customer-order',
      pageId: 'markdown/instances/customer-order__order-4500123.md',
      mode: undefined,
    });
  });

  it('parses palette invocation (undefined)', () => {
    const parsed = parseStartSkillInput(undefined);
    expect(parsed).toEqual({
      skill: undefined,
      pageId: undefined,
      mode: undefined,
    });
  });
});

describe('resolveStartAction', () => {
  it('builds a background capture event with mode run', () => {
    const action = resolveStartAction({
      skill: 'renewal',
      pageId: 'markdown/instances/renewal/c1.md',
      mode: 'background',
      harness: '',
    });
    expect(action.type).toBe('capture');
    if (action.type === 'capture') {
      expect(action.mode).toBe('background');
      expect(action.event.label_skill).toBe('renewal');
      expect(action.event.instance_page_id).toBe('markdown/instances/renewal/c1.md');
      expect(action.event.provenance).toEqual({ manual: { mode: 'run' } });
    }
  });

  it('builds a plan capture event with mode plan', () => {
    const action = resolveStartAction({
      skill: 'renewal',
      pageId: 'markdown/instances/renewal/c1.md',
      mode: 'plan',
      harness: '',
    });
    expect(action.type).toBe('capture');
    if (action.type === 'capture') {
      expect(action.mode).toBe('plan');
      expect(action.event.provenance).toEqual({ manual: { mode: 'plan' } });
    }
  });

  it('passes harness from config into event provenance only when non-empty', () => {
    const actionWithHarness = resolveStartAction({
      skill: 'renewal',
      pageId: 'markdown/instances/renewal/c1.md',
      mode: 'background',
      harness: 'claude-3-5-sonnet',
    });
    if (actionWithHarness.type === 'capture') {
      expect(actionWithHarness.event.provenance).toEqual({
        manual: { mode: 'run', harness: 'claude-3-5-sonnet' },
      });
    }

    const actionBlankHarness = resolveStartAction({
      skill: 'renewal',
      pageId: 'markdown/instances/renewal/c1.md',
      mode: 'background',
      harness: '   ',
    });
    if (actionBlankHarness.type === 'capture') {
      expect(actionBlankHarness.event.provenance).toEqual({
        manual: { mode: 'run' },
      });
    }
  });

  it('delegates terminal mode to escurel.startInTerminal', () => {
    const action = resolveStartAction({
      skill: 'renewal',
      pageId: 'markdown/instances/renewal/c1.md',
      mode: 'terminal',
    });
    expect(action).toEqual({
      type: 'terminal',
      command: 'escurel.startInTerminal',
      args: {
        skill: 'renewal',
        pageId: 'markdown/instances/renewal/c1.md',
      },
    });
  });
});

describe('formatStartError', () => {
  it('formats forbidden as "You are not allowed to start this skill here."', () => {
    const err = new EscurelError('forbidden', 'Forbidden: ACL denied');
    expect(formatStartError(err)).toBe('You are not allowed to start this skill here.');
  });

  it('falls back to describeError for other EscurelError kinds', () => {
    const err = new EscurelError('unauthorized', 'Invalid token');
    expect(formatStartError(err)).toContain('not signed in');
  });

  it('never shows a stack for regular errors', () => {
    const err = new Error('Something went wrong\n    at Object.<anonymous> (/path/file.ts:1:1)');
    const msg = formatStartError(err);
    expect(msg).toContain('Something went wrong');
    expect(msg).not.toContain('at Object');
  });
});

describe('what the notices say', () => {
  // The page is named by its own id, not by the skill being STARTED: a supplier-risk start on a
  // customer-order page showed "customer-order__order-4500123" (the raw file name) in the real window.
  const PAGE = 'markdown/instances/customer-order__order-4500123.md';

  it('names the instance the skill was started on', () => {
    expect(startedMessage('supplier-risk', PAGE)).toBe('Started supplier-risk on order-4500123');
  });

  it('says only the skill when there is no target', () => {
    expect(startedMessage('supplier-risk', '')).toBe('Started supplier-risk');
  });

  it('names the instance in the plan-ready notice too', () => {
    expect(planReadyMessage('supplier-risk', PAGE)).toBe(
      'Plan ready for supplier-risk on order-4500123',
    );
    expect(planReadyMessage('supplier-risk', '')).toBe('Plan ready for supplier-risk');
  });
});
