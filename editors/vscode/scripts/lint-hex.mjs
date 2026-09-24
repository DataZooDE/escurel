// Belt and braces beside the ESLint rule: fail on any literal colour under
// webview/ (also catches .css files the rule never sees).
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';

const COLOUR = /#[0-9a-f]{3,8}\b|\b(?:rgb|hsl)a?\(/i;
const hits = [];
const walk = (dir) => {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) walk(p);
    else if (/\.(ts|js|css|html)$/.test(name)) {
      readFileSync(p, 'utf8')
        .split('\n')
        .forEach((line, i) => {
          const m = COLOUR.exec(line);
          if (m) hits.push(`${p}:${i + 1}: ${m[0]}`);
        });
    }
  }
};
walk('webview');
if (hits.length) {
  console.error('literal colours in webview/ (use --vscode-* tokens):\n' + hits.join('\n'));
  process.exit(1);
}
console.log('lint-hex: webview/ is token-only');
