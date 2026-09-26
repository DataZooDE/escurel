import Mocha from 'mocha';
import { readdirSync } from 'node:fs';
import { join } from 'node:path';

/**
 * The runner suites, which run against their own gateway.
 *
 * They need an inbox holding NOTHING but the trigger they capture: the echo
 * harness folds the oldest inbox event carrying a target instance, which is not
 * necessarily the trigger its run was dispatched for. With the crm-demo seed's
 * pre-seeded events present, a run reaches for one of those instead and the
 * cascade is neither deterministic nor about the page under test.
 */
export function run(): Promise<void> {
  const mocha = new Mocha({ ui: 'tdd', color: true, timeout: 120_000 });
  const grep = process.env.ESCUREL_TEST_GREP;
  if (grep) mocha.grep(new RegExp(grep));
  for (const f of readdirSync(__dirname).filter((f) => f.endsWith('.test.js')))
    mocha.addFile(join(__dirname, f));
  return new Promise((resolve, reject) => {
    mocha.run((failures) =>
      failures ? reject(new Error(`${failures} runner test(s) failed`)) : resolve(),
    );
  });
}
