import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { detectAdminState, type ToolInfo } from '../../src/auth/adminState';

const recorded = (name: string): ToolInfo[] =>
  (
    JSON.parse(readFileSync(join(__dirname, 'fixtures/controls', name), 'utf8')) as {
      result: { tools: ToolInfo[] };
    }
  ).result.tools;

// Recorded from escurel-test-gateway: the same gateway, an admin's token and a human's. The
// gateway filters tools/list by role and tags every tool with its scope, so what the caller
// sees is the one fact about their role the extension can read without a role claim.
describe('detectAdminState', () => {
  it('is admin when the gateway lists admin-scoped tools for this token', () => {
    expect(detectAdminState(recorded('tools-list-admin.json'))).toBe('admin');
  });

  it('is not-admin when every tool declares a scope and none is admin', () => {
    expect(detectAdminState(recorded('tools-list-human.json'))).toBe('not-admin');
  });

  it('does not guess from an empty list', () => {
    expect(detectAdminState([])).toBe('unknown');
  });

  it('does not call someone a non-admin on a gateway that does not tag scopes', () => {
    // An older gateway lists tools without `scope`; no admin tool there proves nothing.
    expect(detectAdminState([{ name: 'expand' }, { name: 'search' }])).toBe('unknown');
  });

  it('is unknown when only some tools declare a scope and none is admin', () => {
    expect(detectAdminState([{ name: 'expand', scope: 'agent' }, { name: 'search' }])).toBe(
      'unknown',
    );
  });
});
