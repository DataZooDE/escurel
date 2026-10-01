import { describe, expect, it } from 'vitest';
import { runnerEnv } from '../integration/runnerEnv';

const info = {
  gateway_url: 'http://127.0.0.1:1',
  issuer_url: 'http://127.0.0.1:2',
  kid: 'k',
  signing_key: 'KEY',
  bearer: 'b',
  tenant: 'vsx',
};

describe('runnerEnv', () => {
  // The runner gives a static ESCUREL_RUNNER_TOKEN precedence over the signing key. A developer
  // who exports one for other work would silently turn this suite from "minted per-run tokens"
  // into "static bearer" — the exact mode that cannot prove which run wrote what — and the
  // lineage assertions would fail for a reason that looks nothing like the cause.
  it('never lets an inherited static token override minted mode', () => {
    const env = runnerEnv({ ESCUREL_RUNNER_TOKEN: 'static', PATH: '/bin' }, info, {
      port: 9,
      dir: '/d',
    });
    expect(env.ESCUREL_RUNNER_TOKEN).toBeUndefined();
    expect(env.PATH).toBe('/bin');
  });

  it('points the runner at the gateway and gives it the issuer it signs against', () => {
    const env = runnerEnv({}, info, { port: 9, dir: '/d' });
    expect(env).toMatchObject({
      ESCUREL_RUNNER_GATEWAY_URL: info.gateway_url,
      ESCUREL_RUNNER_TENANT: 'vsx',
      ESCUREL_RUNNER_AUTH_ISSUER: info.issuer_url,
      ESCUREL_RUNNER_AUTH_KID: 'k',
      ESCUREL_RUNNER_AUTH_SIGNING_KEY: 'KEY',
      ESCUREL_RUNNER_HARNESS: 'echo',
    });
  });
});
