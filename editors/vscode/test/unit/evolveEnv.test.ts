// The Evolve agent's environment is rendered in one place (demo/evolve-env.mjs) for run.sh and the e2e fixtures.
import { spawnSync } from 'node:child_process';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { evolveAgentEnv } from '../../demo/evolve-env.mjs';

const input = {
  gatewayUrl: 'http://127.0.0.1:1',
  adminBearer: 'tok',
  issuerUrl: 'http://127.0.0.1:2/realms/x',
  tenant: 'vsx',
};

describe('evolveAgentEnv', () => {
  it('points the agent at the gateway and verifies tokens against the demo issuer', () => {
    const env = evolveAgentEnv(input);
    expect(env).toMatchObject({
      ESCUREL_ENDPOINT: input.gatewayUrl,
      ESCUREL_TOKEN: 'tok',
      EVOLVE_TENANT: 'vsx',
      EVOLVE_OIDC_AUDIENCE: 'escurel',
      EVOLVE_OIDC_JWKS_URI: `${input.issuerUrl}/protocol/openid-connect/certs`,
    });
  });

  it("passes the agent's OIDC keys under both the old and the new (ESCUREL_EVOLVE_OIDC_*) names", () => {
    const env = evolveAgentEnv(input);
    for (const part of ['ISSUER', 'AUDIENCE', 'JWKS_URI']) {
      expect(env[`ESCUREL_EVOLVE_OIDC_${part}`], part).toBe(env[`ESCUREL_OIDC_${part}`]);
      expect(env[`ESCUREL_OIDC_${part}`], part).toBeTruthy();
    }
  });

  it('prints KEY=VALUE lines for run.sh and refuses incomplete arguments', () => {
    const script = join(__dirname, '../../demo/evolve-env.mjs');
    const ok = spawnSync(
      process.execPath,
      [script, 'lines', input.gatewayUrl, 'tok', input.issuerUrl, 'vsx'],
      { encoding: 'utf8' },
    );
    expect(ok.status).toBe(0);
    expect(ok.stdout.split('\n')).toContain('EVOLVE_TENANT=vsx');
    expect(spawnSync(process.execPath, [script, 'lines'], { encoding: 'utf8' }).status).toBe(2);
  });
});
