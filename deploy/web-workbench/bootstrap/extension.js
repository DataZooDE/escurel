// Web workbench only. NOT part of the shipped extension.
//
// What a person should see when the page loads: the Escurel sidebar and the "Today" board, not an
// empty IDE. It runs commands the extension already exposes and carries no token, no setting that
// holds a secret: the gateway sign-in is the extension's own (OIDC, or none for a gateway without a
// verifier).
const vscode = require('vscode');

exports.activate = async () => {
  // The first load of a browser profile lands on VS Code's default layout (the Explorer); later loads
  // restore what the person last had open, and these are then no-ops.
  await vscode.commands.executeCommand('workbench.view.extension.escurel');
  await vscode.commands.executeCommand('workbench.view.extension.escurel-runner');
  // The calm layout is already baked into the settings; entering focus mode records that, so the board
  // offers "Leave focus view" (put the classic IDE look back) rather than "Switch to focus view".
  await vscode.commands.executeCommand('escurel.focusMode.enter', { silent: true, overview: true });
  // Focus mode puts the activity bar at the top; here it stays hidden, which is what takes the stock
  // Explorer / Search / Source Control / Run / Extensions icons away.
  await vscode.workspace
    .getConfiguration()
    .update('workbench.activityBar.location', 'hidden', vscode.ConfigurationTarget.Global);
};

exports.deactivate = () => {};
