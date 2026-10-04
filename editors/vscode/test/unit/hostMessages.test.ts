import { describe, expect, it } from 'vitest';
import { skillPageMessageAllowed } from '../../src/shared/hostMessages';
import type { SkillPageModel } from '../../src/shared/skillPage';

// A webview is a separate, less-trusted context: the host re-checks every id it is asked to act on
// against what the host itself put on that page.
const skillModel = {
  id: 'order',
  actions: [{ skill: 'credit-check', label: 'Check credit' }],
  instances: { items: [{ pageId: 'markdown/instances/order__1.md', title: 't' }], more: false },
  runs: [{ rootEventId: 'ROOT1', runId: 'RUN1', pageId: 'markdown/instances/order__2.md', title: 'x', at: null, state: 'done' }],
} as unknown as SkillPageModel;

describe('skillPageMessageAllowed', () => {
  it('allows what the page itself offers', () => {
    expect(skillPageMessageAllowed(skillModel, { type: 'start-skill', skill: 'credit-check', mode: 'run' })).toBe(true);
    expect(skillPageMessageAllowed(skillModel, { type: 'open-page', pageId: 'markdown/instances/order__1.md' })).toBe(true);
    expect(skillPageMessageAllowed(skillModel, { type: 'open-page', pageId: 'markdown/instances/order__2.md' })).toBe(true);
    expect(skillPageMessageAllowed(skillModel, { type: 'open-thread', rootEventId: 'ROOT1' })).toBe(true);
    expect(skillPageMessageAllowed(skillModel, { type: 'open-run', runId: 'RUN1' })).toBe(true);
    expect(skillPageMessageAllowed(skillModel, { type: 'refresh' })).toBe(true);
  });
  it('refuses ids the host never showed', () => {
    expect(skillPageMessageAllowed(skillModel, { type: 'start-skill', skill: 'delete-everything', mode: 'run' })).toBe(false);
    expect(skillPageMessageAllowed(skillModel, { type: 'open-page', pageId: 'markdown/skills/secret.md' })).toBe(false);
    expect(skillPageMessageAllowed(skillModel, { type: 'open-thread', rootEventId: 'OTHER' })).toBe(false);
    expect(skillPageMessageAllowed(skillModel, { type: 'open-run', runId: 'OTHER' })).toBe(false);
    expect(skillPageMessageAllowed(undefined, { type: 'open-run', runId: 'RUN1' })).toBe(false);
  });
  it('refuses a mode that is not run or plan', () => {
    expect(skillPageMessageAllowed(skillModel, { type: 'start-skill', skill: 'credit-check', mode: 'terminal' as never })).toBe(false);
  });
});
