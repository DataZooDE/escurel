// Whether the signed-in token is an admin's, against a real gateway that verifies tokens and
// tags every tool with its scope. The recorded lists are what the unit tests use; this proves
// they are what the gateway really says, for two real tokens of the same subject.
import * as assert from 'node:assert/strict';
import type { EscurelApi } from '../../../src/extension';
import { detectAdminState } from '../../../src/auth/adminState';
import { activate, signInAsAdmin } from './support';

suite('who is an admin', () => {
  let api: EscurelApi;

  suiteSetup(async function () {
    this.timeout(120_000);
    if (!process.env.ESCUREL_TEST_ADMIN_BEARER) this.skip();
    api = await activate();
  });

  test('the ordinary bearer is not an admin, and the admin bearer is', async function () {
    this.timeout(60_000);
    assert.equal(detectAdminState(await api.services.client.listTools()), 'not-admin');

    const back = signInAsAdmin(api);
    try {
      await api.services.client.close();
      const tools = await api.services.client.listTools();
      assert.equal(detectAdminState(tools), 'admin');
      assert.ok(tools.some((t) => t.name === 'admin_quota' && t.scope === 'admin'));
    } finally {
      back();
      await api.services.client.close();
    }
    // And back again: signing out of admin must leave nothing admin behind.
    assert.equal(detectAdminState(await api.services.client.listTools()), 'not-admin');
  });
});
