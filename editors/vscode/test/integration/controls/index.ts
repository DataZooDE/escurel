import Mocha from 'mocha';
import { readdirSync } from 'node:fs';
import { join } from 'node:path';

export function run(): Promise<void> {
  const mocha = new Mocha({ ui: 'tdd', color: true, timeout: 120_000 });
  const grep = process.env.ESCUREL_TEST_GREP;
  if (grep) mocha.grep(new RegExp(grep));
  for (const file of readdirSync(__dirname).filter((name) => name.endsWith('.test.js')))
    mocha.addFile(join(__dirname, file));
  return new Promise((resolve, reject) => {
    mocha.run((failures) =>
      failures ? reject(new Error(`${failures} control test(s) failed`)) : resolve(),
    );
  });
}
