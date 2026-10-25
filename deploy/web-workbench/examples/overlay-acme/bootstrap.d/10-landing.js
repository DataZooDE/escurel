// Opens the product's own view after the base bootstrap. Scripts get { vscode, context, firstRun } and no secret.
module.exports = {
  overview: false, // the product lands on its own board, not on the Escurel Overview
  async activate({ vscode, firstRun }) {
    await vscode.commands.executeCommand('workbench.view.extension.escurel');
    if (firstRun) await vscode.commands.executeCommand('workbench.action.closePanel');
  },
};
