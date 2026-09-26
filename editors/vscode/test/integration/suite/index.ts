import Mocha from 'mocha';
import { readdirSync } from 'node:fs';
import { join } from 'node:path';

export function run(): Promise<void> {
  const mocha = new Mocha({ ui: 'tdd', color: true, timeout: 60_000 });
  // `ESCUREL_TEST_GREP=cascade npm run test:integration` runs one suite. A real
  // VS Code takes half a minute to start, so narrowing beats re-running all of
  // it while chasing one failure.
  const grep = process.env.ESCUREL_TEST_GREP;
  if (grep) mocha.grep(new RegExp(grep));
  for (const f of readdirSync(__dirname).filter((f) => f.endsWith('.test.js')))
    mocha.addFile(join(__dirname, f));
  return new Promise((resolve, reject) => {
    mocha.run((failures) =>
      failures ? reject(new Error(`${failures} test(s) failed`)) : resolve(),
    );
  });
}
