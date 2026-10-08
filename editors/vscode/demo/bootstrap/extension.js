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
  // The views were drawn BEFORE there was a token (they showed 'not signed in'), and applying a
  // static token does not announce itself the way a real sign-in does. Ask them to load again.
  await vscode.commands.executeCommand('escurel.refresh');
  setInterval(apply, 30_000);

  // The Scenarios view (Anofox Evolve) is not part of this demo's story and, unconfigured, only says it
  // needs an endpoint: leave it out unless the demo starts Evolve (ESCUREL_DEMO_EVOLVE_ENDPOINT).
  await vscode.commands.executeCommand(
    'setContext',
    'escurel.hideScenarios',
    !process.env.ESCUREL_DEMO_EVOLVE_ENDPOINT,
  );

  // The Runner view lives in the right-hand (secondary) sidebar: show it, then the left one.
  await vscode.commands.executeCommand('workbench.view.extension.escurel-runner');
  await vscode.commands.executeCommand('workbench.view.extension.escurel');
  // The calm window (ESCUREL_DEMO_FOCUS=0 keeps the classic look): focus mode on without asking, and the
  // overview board as the first screen instead of one thread.
  if (process.env.ESCUREL_DEMO_FOCUS !== '0') {
    await vscode.commands.executeCommand('escurel.focusMode.enter', {
      silent: true,
      overview: true,
    });
    return;
  }
  if (storyFile) {
    try {
      const story = JSON.parse(fs.readFileSync(storyFile, 'utf8'));
      if (story.rootA) await vscode.commands.executeCommand('escurel.openThread', story.rootA);
      // Optional: also open one page, to land the walkthrough on it (or to look at it).
      if (process.env.ESCUREL_DEMO_OPEN_PAGE) {
        await vscode.commands.executeCommand(
          'escurel.openInstance',
          process.env.ESCUREL_DEMO_OPEN_PAGE,
        );
      }
    } catch {
      /* no story yet: the window is still usable */
    }
  }
};
