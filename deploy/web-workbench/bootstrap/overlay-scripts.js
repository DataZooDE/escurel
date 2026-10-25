// Runs the downstream overlay's bootstrap.d/*.js (OVERLAY.md). Web workbench only; carries no credential.
// Scripts get the vscode API and the extension context, nothing else; one that throws or hangs is skipped.
const fs = require('fs');
const path = require('path');

const overlayDir = () => process.env.WORKBENCH_OVERLAY_DIR || '/opt/escurel/overlay';

// Overlay settings.json is validated at build and start (overlay.mjs); here it is only read for one value.
function overlaySetting(key) {
  try {
    return JSON.parse(fs.readFileSync(path.join(overlayDir(), 'settings.json'), 'utf8'))[key];
  } catch {
    return undefined;
  }
}

function loadScripts() {
  const dir = path.join(overlayDir(), 'bootstrap.d');
  let names = [];
  try {
    names = fs.readdirSync(dir).filter((n) => n.endsWith('.js')).sort();
  } catch {
    return [];
  }
  const scripts = [];
  for (const name of names) {
    try {
      const m = require(path.join(dir, name));
      if (typeof m.activate !== 'function') throw new Error('exports no activate function');
      scripts.push({ name, overview: m.overview, activate: m.activate });
    } catch (e) {
      console.error(`escurel-web bootstrap: overlay script ${name} skipped: ${e && e.message}`);
    }
  }
  return scripts;
}

async function runScripts(scripts, vscode, context, firstRun) {
  const limit = Number(process.env.WORKBENCH_OVERLAY_SCRIPT_TIMEOUT_MS) || 10000;
  for (const s of scripts) {
    let timer;
    try {
      await Promise.race([
        s.activate({ vscode, context, firstRun }),
        new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(`timed out after ${limit} ms`)), limit); }),
      ]);
    } catch (e) {
      console.error(`escurel-web bootstrap: overlay script ${s.name} failed: ${e && e.message}`);
    } finally {
      clearTimeout(timer);
    }
  }
}

module.exports = { loadScripts, runScripts, overlaySetting };
