// Demo only. NOT part of the shipped extension.
//
// Signs the escurel extension in with the bearer a demo gateway keeps fresh in a file, and opens
// the sidebar and the finished thread so the window is ready the moment it appears. It uses the
// same seam the integration suite uses: a method on the API `activate()` returns, so nothing in a
// running install can be handed a credential this way.
const vscode = require('vscode');
const fs = require('node:fs');

exports.activate = async () => {
  const file = process.env.ESCUREL_DEMO_BEARER_FILE;
  const storyFile = process.env.ESCUREL_DEMO_STORY;
  if (!file) return;
  const api = await vscode.extensions.getExtension('datazoo.escurel').activate();

  let applied;
  const apply = () => {
    try {
      const { bearer } = JSON.parse(fs.readFileSync(file, 'utf8'));
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
  setInterval(apply, 30_000);

  // The right-hand sidebar is empty until the Runner view exists; a blank panel is distracting.
  await vscode.commands.executeCommand('workbench.action.closeAuxiliaryBar');
  await vscode.commands.executeCommand('workbench.view.extension.escurel');
  if (storyFile) {
    try {
      const story = JSON.parse(fs.readFileSync(storyFile, 'utf8'));
      if (story.rootA) await vscode.commands.executeCommand('escurel.openThread', story.rootA);
    } catch {
      /* no story yet: the window is still usable */
    }
  }
};
