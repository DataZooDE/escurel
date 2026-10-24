// Web workbench only. NOT part of the shipped extension.
//
// What a person should see when the page loads: the Escurel sidebar and the "Today" board, not an
// empty IDE. It runs commands the extension already exposes and carries no token, no setting that
// holds a secret: the gateway sign-in is the extension's own (OIDC, or none for a gateway without a
// verifier).
//
// The calm layout is applied ONCE per browser profile. A person who leaves focus mode keeps the classic
// layout on every later load: the first-run flag lives in the extension's global state, which sits in the
// workbench's data volume.
const vscode = require('vscode');
const overlay = require('./overlay-scripts');

const FIRST_RUN = 'escurel.web.bootstrapped';

exports.activate = async (context) => {
  // A downstream image may add scripts (OVERLAY.md). They run last, on every load.
  const scripts = overlay.loadScripts();
  const firstRun = !(context && context.globalState.get(FIRST_RUN));
  try {
    await base(context, scripts);
  } finally {
    await overlay.runScripts(scripts, vscode, context, firstRun);
  }
};

async function base(context, scripts) {
  // The views are revealed on every load (a no-op when they are already open).
  await vscode.commands.executeCommand('workbench.view.extension.escurel');
  await vscode.commands.executeCommand('workbench.view.extension.escurel-runner');
  if (context && context.globalState.get(FIRST_RUN)) return;
  // The calm layout is already baked into the settings; entering focus mode records that, so the board
  // offers "Leave focus view" (put the classic IDE look back) rather than "Switch to focus view".
  await vscode.commands.executeCommand('escurel.focusMode.enter', { silent: true, overview: !scripts.some((x) => x.overview === false) });
  // Focus mode puts the activity bar at the top; here it stays hidden, which is what takes the stock
  // Explorer / Search / Source Control / Run / Extensions icons away. An overlay's settings.json may say otherwise.
  await vscode.workspace
    .getConfiguration()
    .update('workbench.activityBar.location', overlay.overlaySetting('workbench.activityBar.location') || 'hidden', vscode.ConfigurationTarget.Global);
  if (context) await context.globalState.update(FIRST_RUN, true);
}

exports.deactivate = () => {};
