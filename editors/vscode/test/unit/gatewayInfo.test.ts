import { describe, expect, it } from 'vitest';
import { parseGatewayInfo } from '../integration/gatewayInfo';

const line = {
  gateway_url: 'http://127.0.0.1:1',
  issuer_url: 'http://127.0.0.1:2',
  kid: 'k',
  signing_key: 'KEY',
  bearer: 'b',
  admin_bearer: 'a',
  tenant: 'vsx',
};

// The harness reads ONE line from escurel-test-gateway. A binary built before a field existed
// prints a line without it, and a suite that needs that field would skip itself and look green.
// So a missing field is an error that names the field and says what to do.
describe('parseGatewayInfo', () => {
  it('accepts the full line', () => {
    expect(parseGatewayInfo(JSON.stringify(line)).admin_bearer).toBe('a');
  });

  it('refuses a binary that predates a field, naming it and the fix', () => {
    const old: Partial<typeof line> = { ...line };
    delete old.admin_bearer;
    expect(() => parseGatewayInfo(JSON.stringify(old))).toThrow(/admin_bearer.*rebuild/is);
  });

  it('refuses an empty field as well as a missing one', () => {
    expect(() => parseGatewayInfo(JSON.stringify({ ...line, bearer: '' }))).toThrow(/bearer/);
  });

  it('says what it received when the line is not JSON', () => {
    expect(() => parseGatewayInfo('error: something')).toThrow(/not JSON.*error: something/is);
  });
});
