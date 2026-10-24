// The downstream overlay contract (OVERLAY.md): loading, validation and the pure merges.
// Used by render-settings.mjs and the entrypoint at container start, and by `escurel-overlay-install` at build.
//
//   node overlay.mjs check   [dir]      validate; exit 64 with all problems on failure
//   node overlay.mjs get KEY [dir]      print productName | welcomeText (empty when unset)
//   node overlay.mjs install [dir]      build time, as root: apply the overlay to code-server and the extension dir
import { spawnSync } from 'node:child_process';
import { existsSync, lstatSync, readdirSync, readFileSync } from 'node:fs';
import { basename, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { USED } from './login/used-colors.mjs';

export const OVERLAY_DIR = '/opt/escurel/overlay';
const KiB = 1024;
export const LIMITS = { json: 256 * KiB, css: 256 * KiB, script: 64 * KiB, asset: 2048 * KiB, vsix: 64 * 1024 * KiB };
const TOP = new Set(['settings.json', 'workbench.css', 'login.css', 'login-tokens.json', 'product.json', 'assets', 'extensions', 'bootstrap.d']);
const ASSET_SUFFIX = /\.(woff2|woff|ttf|svg|png|ico)$/i;
const PRODUCT_KEYS = ['nameShort', 'nameLong'];
const SETTING_NAME = /^(\[[^\]\s]+\]|[A-Za-z][\w-]*(\.[\w-]+)+|[A-Za-z][\w-]*)$/;
// Settings a downstream layer may not set: trust and security, the terminal, the gallery and updates, telemetry,
// and the values the environment owns (render-settings.mjs).
const DENIED = [
  /trust/i, /^security\./i, /^terminal\.integrated\./, /^extensions\.(gallery|autoUpdate|autoCheckUpdates|allowed|ignoreRecommendations)/,
  /^update\./, /^telemetry\./, /^escurel\.(gatewayUrl|auth\.)/, /^remote\./, /^task\./,
];
const SCRIPT_FORBIDDEN = [
  [/child_process/, 'child_process'], [/process\s*\.\s*env/, 'process.env'], [/\bfetch\s*\(/, 'fetch'],
  [/XMLHttpRequest|WebSocket/, 'network API'], [/require\s*\(\s*['"](node:)?(https?|net|tls|dgram|dns|http2)['"]/, 'network module'],
  [/\beval\s*\(|new\s+Function\s*\(/, 'eval'],
];

export class OverlayError extends Error {
  constructor(problems) {
    super(`overlay: ${problems.join('; ')}`);
    this.problems = problems;
  }
}

const isPlainObject = (v) => v !== null && typeof v === 'object' && !Array.isArray(v);

export function loadOverlay(dir = OVERLAY_DIR) {
  const out = {
    present: false, settings: {}, workbenchCss: '', loginCss: '', loginTokens: {}, product: {},
    assets: [], extensions: [], bootstrap: [],
  };
  if (!existsSync(dir)) return out;
  out.present = true;
  const problems = [];
  const bad = (file, msg) => problems.push(`${file}: ${msg}`);

  // lstat first: a symlink or non-regular file is refused, then the size limit.
  function readChecked(rel, limit) {
    const full = join(dir, rel);
    const st = lstatSync(full);
    if (st.isSymbolicLink()) return bad(rel, 'symlink not allowed'), undefined;
    if (!st.isFile()) return bad(rel, 'not a regular file'), undefined;
    if (st.size > limit) return bad(rel, `${st.size} bytes exceeds the limit of ${limit}`), undefined;
    return full;
  }
  function json(rel) {
    const full = readChecked(rel, LIMITS.json);
    if (!full) return undefined;
    let v;
    try {
      v = JSON.parse(readFileSync(full, 'utf8'));
    } catch (e) {
      return bad(rel, `not valid JSON (${e.message})`), undefined;
    }
    if (!isPlainObject(v)) return bad(rel, 'must be a JSON object'), undefined;
    return v;
  }
  function css(rel) {
    const full = readChecked(rel, LIMITS.css);
    if (!full) return '';
    const text = readFileSync(full, 'utf8');
    if (/@import/i.test(text)) bad(rel, '@import is not allowed');
    if (/url\(\s*['"]?\s*(https?:|\/\/|data:text|javascript:)/i.test(text)) bad(rel, 'external url() is not allowed');
    if (rel === 'login.css' && /#[0-9a-f]{3,8}\b|\b(rgb|rgba|hsl|hsla|hwb|oklch|lab)\(/i.test(text)) {
      bad(rel, 'colour literal found: put colours in login-tokens.json and use var(--vscode-*)');
    }
    return text;
  }
  function list(sub, ok, limit, what) {
    const base = join(dir, sub);
    if (!existsSync(base)) return [];
    if (!lstatSync(base).isDirectory()) return bad(sub, 'must be a directory'), [];
    const names = [];
    for (const name of readdirSync(base).sort()) {
      const rel = `${sub}/${name}`;
      if (!ok.test(name)) {
        bad(rel, `not a ${what}`);
        continue;
      }
      if (readChecked(rel, limit)) names.push(join(base, name));
    }
    return names;
  }

  for (const name of readdirSync(dir)) {
    if (!TOP.has(name)) bad(name, `unknown entry ${name}`);
  }

  if (existsSync(join(dir, 'settings.json'))) {
    const s = json('settings.json');
    if (s) {
      for (const key of Object.keys(s)) {
        if (!SETTING_NAME.test(key)) bad('settings.json', `"${key}" is not a setting name`);
        else if (DENIED.some((re) => re.test(key))) bad('settings.json', `"${key}" is not allowed (trust, security, terminal, gallery, update, telemetry and environment-owned settings are fixed by the base image)`);
      }
      out.settings = s;
    }
  }
  if (existsSync(join(dir, 'workbench.css'))) out.workbenchCss = css('workbench.css');
  if (existsSync(join(dir, 'login.css'))) out.loginCss = css('login.css');

  if (existsSync(join(dir, 'login-tokens.json'))) {
    const t = json('login-tokens.json');
    if (t) {
      for (const key of Object.keys(t)) if (!['productName', 'welcomeText', 'colors'].includes(key)) bad('login-tokens.json', `unknown key ${key}`);
      for (const k of ['productName', 'welcomeText']) {
        if (k in t && !(typeof t[k] === 'string' && /^[^<>&"'`{}\\\n]{1,60}$/.test(t[k]))) bad('login-tokens.json', `${k} must be 1-60 characters without markup or quotes`);
      }
      if ('colors' in t) {
        if (!isPlainObject(t.colors)) bad('login-tokens.json', 'colors must be an object');
        else {
          for (const [id, v] of Object.entries(t.colors)) {
            if (!USED.includes(id)) bad('login-tokens.json', `colour id ${id} is not used by the login pages (known: ${USED.join(', ')})`);
            else if (typeof v !== 'string' || !/^#([0-9a-f]{3}|[0-9a-f]{6}|[0-9a-f]{8})$/i.test(v)) bad('login-tokens.json', `colour ${id} must be #rgb, #rrggbb or #rrggbbaa, got ${JSON.stringify(v)}`);
          }
        }
      }
      out.loginTokens = t;
    }
  }

  if (existsSync(join(dir, 'product.json'))) {
    const p = json('product.json');
    if (p) {
      for (const [k, v] of Object.entries(p)) {
        if (!PRODUCT_KEYS.includes(k)) bad('product.json', `key ${k} is not allowed (allowed: ${PRODUCT_KEYS.join(', ')})`);
        else if (typeof v !== 'string' || !/^[^<>"'`\\\n]{1,60}$/.test(v)) bad('product.json', `${k} must be 1-60 characters without markup or quotes`);
      }
      out.product = p;
    }
  }

  out.assets = list('assets', ASSET_SUFFIX, LIMITS.asset, 'font or image file');
  out.extensions = list('extensions', /\.vsix$/i, LIMITS.vsix, '.vsix file');
  out.bootstrap = list('bootstrap.d', /\.js$/, LIMITS.script, '.js file');
  for (const file of out.bootstrap) {
    const text = readFileSync(file, 'utf8');
    for (const [re, what] of SCRIPT_FORBIDDEN) if (re.test(text)) bad(`bootstrap.d/${basename(file)}`, `forbidden: ${what}`);
  }

  if (problems.length) throw new OverlayError(problems);
  return out;
}

// ---- pure merges -----------------------------------------------------------------------------------

export const mergeSettings = (base, overlay) => ({ ...base, ...overlay });
export const patchProduct = (product, patch) => ({ ...product, ...patch });

const MARK = (what) => `\n/* escurel overlay: ${what} */\n`;
export function composeWorkbenchCss(baseCss, overlayCss) {
  if (!overlayCss) return baseCss;
  return `${baseCss}${baseCss.endsWith('\n') ? '' : '\n'}${MARK('workbench.css').trimStart()}${overlayCss}${overlayCss.endsWith('\n') ? '' : '\n'}`;
}
export function composeLoginCss(baseCss, overlayCss) {
  if (!overlayCss) return baseCss;
  return `${baseCss}${baseCss.endsWith('\n') ? '' : '\n'}${MARK('login.css').trimStart()}${overlayCss}${overlayCss.endsWith('\n') ? '' : '\n'}`;
}
// The login pages' theme overrides: the same `--vscode-*` names gen-global-css.mjs emits, redefined after them.
export function loginTokensCss(tokens) {
  const rows = Object.entries(tokens.colors ?? {}).map(([id, v]) => `  --vscode-${id.replaceAll('.', '-')}: ${v};`);
  return rows.length ? `${MARK('login-tokens.json').trimStart()}:root {\n${rows.join('\n')}\n}\n` : '';
}

// ---- CLI -------------------------------------------------------------------------------------------

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const [cmd, a, b] = process.argv.slice(2);
  const fail = (e) => {
    console.error(`escurel-web: ${e instanceof OverlayError ? e.message : `overlay: ${e.message}`}`);
    process.exit(64);
  };
  try {
    if (cmd === 'check') loadOverlay(a ?? OVERLAY_DIR);
    else if (cmd === 'get') console.log(loadOverlay(b ?? OVERLAY_DIR).loginTokens[a] ?? '');
    else if (cmd === 'install') {
      // overlay-install.mjs imports this module, so it runs as its own entry point: a dynamic import back into it
      // is a cycle through this module's top-level await and never settles.
      const r = spawnSync(process.execPath, [fileURLToPath(new URL('./overlay-install.mjs', import.meta.url)), ...(a ? [a] : [])], {
        stdio: 'inherit',
      });
      process.exit(r.status ?? 1);
    } else {
      console.error('usage: overlay.mjs check|get KEY|install [dir]');
      process.exit(2);
    }
  } catch (e) {
    fail(e);
  }
}
