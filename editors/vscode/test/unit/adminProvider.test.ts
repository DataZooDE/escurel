import { describe, expect, it } from 'vitest';
import { AdminStateProvider } from '../../src/auth/adminProvider';
import type { ToolInfo } from '../../src/auth/adminState';

const admin: ToolInfo[] = [
  { name: 'a', scope: 'agent' },
  { name: 'admin_quota', scope: 'admin' },
];
const human: ToolInfo[] = [{ name: 'a', scope: 'agent' }];

function provider(answers: (ToolInfo[] | Error)[]) {
  let calls = 0;
  const p = new AdminStateProvider(async () => {
    const a = answers[Math.min(calls, answers.length - 1)]!;
    calls += 1;
    if (a instanceof Error) throw a;
    return a;
  });
  return { p, calls: () => calls };
}

describe('AdminStateProvider', () => {
  it('asks the gateway once and remembers the answer', async () => {
    const { p, calls } = provider([admin]);
    expect(await p.get()).toBe('admin');
    expect(await p.get()).toBe('admin');
    expect(calls()).toBe(1);
  });

  it('asks again after the token changed, and says so', async () => {
    const { p, calls } = provider([human, admin]);
    expect(await p.get()).toBe('not-admin');
    let fired = 0;
    p.onDidChange(() => (fired += 1));
    p.invalidate();
    expect(fired).toBe(1);
    expect(await p.get()).toBe('admin');
    expect(calls()).toBe(2);
  });

  it('answers unknown when the gateway cannot be asked, and does not remember the failure', async () => {
    const { p, calls } = provider([new Error('offline'), human]);
    expect(await p.get()).toBe('unknown');
    expect(await p.get()).toBe('not-admin');
    expect(calls()).toBe(2);
  });

  it('shares one request between callers that ask at the same time', async () => {
    const { p, calls } = provider([admin]);
    const [a, b] = await Promise.all([p.get(), p.get()]);
    expect([a, b]).toEqual(['admin', 'admin']);
    expect(calls()).toBe(1);
  });
});
