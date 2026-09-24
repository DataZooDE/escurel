import { describe, expect, it } from 'vitest';
import { readConfig } from '../../src/config';

describe('scaffold', () => {
  it('reads config with defaults when nothing is set', () => {
    const c = readConfig();
    expect(c.gatewayUrl).toBe('');
    expect(c.shellHarness).toBe('claude');
    expect(c.auth.scopes).toContain('openid');
  });
});
