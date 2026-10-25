// Build-time half of the overlay contract (OVERLAY.md): `escurel-overlay-install`, run as root in the
// downstream Dockerfile after `COPY overlay/ /opt/escurel/overlay/`.
//
// Without an overlay it still asserts that every file of the pinned code-server that the overlay would touch
// exists, so a code-server upgrade that moves one fails the base build instead of silently shipping the stock page.
import { copyFileSync, existsSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { basename, dirname, join } from 'node:path';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import {
  composeLoginCss, composeWorkbenchCss, loadOverlay, loginTokensCss, OVERLAY_DIR, OverlayError, patchProduct,
} from './overlay.mjs';

const CS = '/usr/lib/code-server';
const OPT = '/opt/escurel';

export const defaultTargets = () => ({
  workbenchCss: `${CS}/lib/vscode/out/vs/code/browser/workbench/workbench.css`,
  productJson: `${CS}/lib/vscode/product.json`,
  pagesDir: `${CS}/src/browser/pages`,
  installExtension(vsix) {
    // The same invocation as the base Dockerfile: extensions live in the image, not in a volume.
    execFileSync('/usr/bin/code-server', [
      '--user-data-dir', `${OPT}/ud-overlay`, '--extensions-dir', `${OPT}/extensions`, '--install-extension', vsix,
    ], { stdio: 'inherit', env: { ...process.env, HOME: '/tmp' } });
  },
  cleanup() {
    rmSync(`${OPT}/ud-overlay`, { recursive: true, force: true });
  },
});

export function install(overlay, t = defaultTargets()) {
  const fail = (m) => {
    throw new Error(m);
  };
  const need = (f, what) => existsSync(f) || fail(`${what} ${f} not found: the pinned code-server moved it`);
  need(t.workbenchCss, 'workbench.css');
  need(t.productJson, 'product.json');
  for (const f of ['login.html', 'login.css', 'global.css', 'error.css']) {
    if (f !== 'error.css' || existsSync(join(t.pagesDir, f))) need(join(t.pagesDir, f), `login page ${f}`);
  }
  if (!overlay.present) return;

  const wbDir = dirname(t.workbenchCss);
  const css = readFileSync(t.workbenchCss, 'utf8');
  if (css.includes('escurel overlay:')) fail('overlay already applied to workbench.css');

  // Check what can fail before anything is written.
  const loginHtml = join(t.pagesDir, 'login.html');
  let html = readFileSync(loginHtml, 'utf8');
  const name = overlay.loginTokens.productName;
  if (name !== undefined && !/<span>Escurel<\/span>/.test(html)) fail('login.html has no <span>Escurel</span> in its titlebar to carry the product name');

  if (overlay.workbenchCss) writeFileSync(t.workbenchCss, composeWorkbenchCss(css, overlay.workbenchCss));
  for (const a of overlay.assets) {
    copyFileSync(a, join(wbDir, basename(a)));
    copyFileSync(a, join(t.pagesDir, basename(a)));
  }
  const globalCss = join(t.pagesDir, 'global.css');
  const tokens = loginTokensCss(overlay.loginTokens);
  if (tokens) writeFileSync(globalCss, readFileSync(globalCss, 'utf8') + (readFileSync(globalCss, 'utf8').endsWith('\n') ? '' : '\n') + tokens);
  const loginCss = join(t.pagesDir, 'login.css');
  if (overlay.loginCss) writeFileSync(loginCss, composeLoginCss(readFileSync(loginCss, 'utf8'), overlay.loginCss));
  if (name !== undefined) writeFileSync(loginHtml, html.replace('<span>Escurel</span>', `<span>${name}</span>`));
  if (Object.keys(overlay.product).length) {
    const p = JSON.parse(readFileSync(t.productJson, 'utf8'));
    writeFileSync(t.productJson, `${JSON.stringify(patchProduct(p, overlay.product), null, 2)}\n`);
  }
  for (const v of overlay.extensions) t.installExtension(v);
  t.cleanup?.();
}

// Entry point: `node overlay-install.mjs [overlay-dir]`, run by the `escurel-overlay-install` wrapper (via overlay.mjs).
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  try {
    install(loadOverlay(process.argv[2] ?? OVERLAY_DIR));
  } catch (e) {
    console.error(`escurel-web: ${e instanceof OverlayError ? e.message : `overlay: ${e.message}`}`);
    process.exit(64);
  }
}
