// Web workbench only. NOT part of the shipped extension.
//
// What a person should see when the page loads: the Escurel sidebar and the "Today" board, not an
// empty IDE. It runs two commands the extension already exposes and carries no token, no setting
// and no secret: the gateway sign-in is the extension's own (OIDC, or none for a gateway without a
// verifier).
const vscode = require('vscode');

exports.activate = async () => {
  // The first load of a browser profile lands on VS Code's default layout (the Explorer); later loads
  // restore what the person last had open, and these two are then no-ops.
  await vscode.commands.executeCommand('workbench.view.extension.escurel');
  await vscode.commands.executeCommand('workbench.view.extension.escurel-runner');
  await vscode.commands.executeCommand('escurel.openOverview');
};

exports.deactivate = () => {};
