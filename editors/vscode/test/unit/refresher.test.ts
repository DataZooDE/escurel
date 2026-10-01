import { describe, expect, it } from 'vitest';
import { TokenRefresher, type StoredSession } from '../../src/auth/refresher';

const now = () => Math.floor(Date.now() / 1000);
const token = (exp: number, tag: string) =>
  `h.${Buffer.from(JSON.stringify({ exp })).toString('base64url')}.${tag}`;

function session(expIn: number, tag = 'a'): StoredSession {
  return {
    accessToken: token(now() + expIn, tag),
    refreshToken: `rt-${tag}`,
    subject: 'alice',
    issuer: 'https://i',
    clientId: 'c',
  };
}

describe('TokenRefresher', () => {
  it('none mode: no session → no token, and refresh is never called', async () => {
    let called = 0;
    const r = new TokenRefresher({
      load: async () => undefined,
      save: async () => {},
      refresh: async () => (called++, session(100)),
    });
    expect(await r.get()).toBeUndefined();
    expect(called).toBe(0);
  });

  it('returns the stored token while it is fresh', async () => {
    const s = session(600);
    const r = new TokenRefresher({
      load: async () => s,
      save: async () => {},
      refresh: async () => session(600, 'b'),
    });
    expect(await r.get()).toBe(s.accessToken);
  });

  it('refreshes ahead of expiry (the verifier has zero leeway), once for concurrent callers, and notifies', async () => {
    let refreshes = 0;
    let saved: StoredSession | undefined;
    const fresh = session(600, 'b');
    const r = new TokenRefresher({
      load: async () => session(30, 'a'),
      save: async (s) => {
        saved = s;
      },
      refresh: async () => {
        refreshes++;
        await new Promise((res) => setTimeout(res, 20));
        return fresh;
      },
      skewSeconds: 60,
    });
    const notified: (string | undefined)[] = [];
    r.onDidRefresh((t) => notified.push(t));
    const [a, b, c] = await Promise.all([r.get(), r.get(), r.get()]);
    expect(refreshes).toBe(1);
    expect(a).toBe(fresh.accessToken);
    expect(b).toBe(a);
    expect(c).toBe(a);
    expect(saved).toEqual(fresh);
    expect(notified).toEqual([fresh.accessToken]);
  });

  it('a failed refresh clears the session and reports undefined', async () => {
    let cleared = false;
    const r = new TokenRefresher({
      load: async () => session(10, 'a'),
      save: async (s) => {
        if (!s) cleared = true;
      },
      refresh: async () => {
        throw new Error('invalid_grant');
      },
      skewSeconds: 60,
    });
    expect(await r.get()).toBeUndefined();
    expect(cleared).toBe(true);
  });
});

describe('TokenRefresher.useStaticToken', () => {
  // The integration suite signs in with a bearer from a test issuer. The seam is a method on
  // an object only code holding the extension's API can reach: no environment variable and no
  // setting reads a credential, so a running install has no way to be handed one.
  const never = () => {
    throw new Error('the store must not be consulted once a static token is set');
  };
  const refresher = () =>
    new TokenRefresher({
      load: async () => never(),
      save: async () => never(),
      refresh: async () => never(),
    });

  it('answers with the token and the subject, without touching the store', async () => {
    const r = refresher();
    r.useStaticToken('bearer-1', 'alice');
    expect(await r.get()).toBe('bearer-1');
    expect(await r.subject()).toBe('alice');
  });

  it('tells long-lived sockets, so they reconnect with the new bearer', async () => {
    const r = refresher();
    const seen: (string | undefined)[] = [];
    r.onDidRefresh((t) => seen.push(t));
    r.useStaticToken('bearer-1', 'alice');
    r.useStaticToken('bearer-2', 'alice');
    expect(seen).toEqual(['bearer-1', 'bearer-2']);
  });

  it('does not try to refresh a token that cannot be refreshed', async () => {
    const r = refresher();
    r.useStaticToken('bearer-1', 'alice');
    expect(await r.invalidate()).toBe('bearer-1');
  });
});
