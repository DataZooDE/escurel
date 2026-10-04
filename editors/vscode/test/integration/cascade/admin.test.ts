// Whether the signed-in token is an admin's, against a real gateway that verifies tokens and
// tags every tool with its scope. The recorded lists are what the unit tests use; this proves
// they are what the gateway really says, for two real tokens of the same subject.
import * as assert from 'node:assert/strict';
import type { EscurelApi } from '../../../src/extension';
import { detectAdminState } from '../../../src/auth/adminState';
import { activate, signInAsAdmin } from './support';
import { requireEnv } from '../requireEnv';

suite('who is an admin', () => {
  let api: EscurelApi;

  suiteSetup(async function () {
    this.timeout(120_000);
    requireEnv(this, 'ESCUREL_TEST_ADMIN_BEARER');
    api = await activate();
  });

  test('the ordinary bearer is not an admin, and the admin bearer is', async function () {
    this.timeout(60_000);
    // Through the provider every surface will use, not the client: signing in as someone else
    // must be noticed with nobody clearing a cache by hand.
    assert.equal(await api.services.admin.get(), 'not-admin');
    const tools = await api.services.client.listTools();
    assert.equal(detectAdminState(tools), 'not-admin');

    const back = signInAsAdmin(api);
    try {
      assert.equal(await api.services.admin.get(), 'admin');
      const asAdmin = await api.services.client.listTools();
      assert.ok(asAdmin.some((t) => t.name === 'admin_quota' && t.scope === 'admin'));
    } finally {
      back();
    }
    // And back again: signing out of admin must leave nothing admin behind.
    assert.equal(await api.services.admin.get(), 'not-admin');
  });
});
