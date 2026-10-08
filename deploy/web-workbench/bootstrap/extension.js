// Web workbench only. NOT part of the shipped extension.
//
// What a person should see when the page loads: the Escurel sidebar and the "Today" board, not an
// empty IDE. It runs commands the extension already exposes and carries no token and no setting that
// holds a secret: the gateway sign-in is the extension's own (OIDC, or none for a gateway without a
// verifier). The one exception is the S2D demo stack, see the bearer file below.
const vscode = require('vscode');
const fs = require('node:fs');

exports.activate = async () => {
  // A demo gateway that verifies tokens (the S2D stack): sign the extension in with the USER bearer the gateway
  // keeps fresh in a read-only file. It is a short-lived token for a synthetic demo tenant; the admin bearer and
  // the issuer's key are never in this container. Without the file (a real deployment) nothing happens here:
  // the sign-in is the extension's own.
  const bearerFile = process.env.WORKBENCH_BEARER_FILE;
  if (bearerFile) {
    const api = await vscode.extensions.getExtension('datazoo.escurel').activate();
    let applied;
    const apply = () => {
      try {
        const { bearer } = JSON.parse(fs.readFileSync(bearerFile, 'utf8'));
        // Only on change: setting a token reconnects every live socket.
        if (bearer && bearer !== applied) {
          applied = bearer;
          api.services.auth.refresher.useStaticToken(bearer, 'alice');
        }
      } catch {
        /* the file is replaced by rename; a read in between simply tries again next time */
      }
    };
    apply();
    await vscode.commands.executeCommand('escurel.refresh');
    setInterval(apply, 20_000);
  }
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
