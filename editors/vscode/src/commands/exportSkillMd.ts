import * as vscode from 'vscode';
import type { Services } from '../services';
import { describeError } from '../errors';
import { readPageMarkdown } from '../fs/read';
import { SKILL_FILE, exportSkillMd } from '../skills/skillMd';

/** What the command is invoked with: a page id, an `escurel:` skill URI (editor title), or a tree row. */
type Target = string | vscode.Uri | { skill?: { id: string } | string } | undefined;

function skillPageId(arg: Target): string | undefined {
  if (typeof arg === 'string')
    return arg.startsWith('markdown/') ? arg : `markdown/skills/${arg}.md`;
  if (arg instanceof vscode.Uri)
    return arg.scheme === 'escurel' ? arg.path.replace(/^\//, 'markdown/') : undefined;
  const skill = arg?.skill;
  if (typeof skill === 'string') return `markdown/skills/${skill}.md`;
  if (skill) return `markdown/skills/${skill.id}.md`;
  const active = vscode.window.activeTextEditor?.document.uri;
  return active?.scheme === 'escurel' ? active.path.replace(/^\//, 'markdown/') : undefined;
}

/**
 * `escurel.exportSkillMd`: write a skill page as `<folder>/<skill>/SKILL.md` for an agent that reads
 * Anthropic-style skills (`references/12`). `folder` skips the picker (tests, other extensions).
 * Returns the written file's URI, or undefined when nothing was written.
 */
export function registerExportSkillMd(context: vscode.ExtensionContext, services: Services): void {
  context.subscriptions.push(
    vscode.commands.registerCommand(
      'escurel.exportSkillMd',
      async (arg?: Target, folder?: vscode.Uri): Promise<vscode.Uri | undefined> => {
        const pageId = skillPageId(arg);
        if (!pageId?.startsWith('markdown/skills/')) {
          void vscode.window.showInformationMessage('Open a skill page first, then export it.');
          return undefined;
        }
        try {
          const page = await readPageMarkdown(services.client, pageId);
          if (!page) throw new Error(`no page ${pageId}`);
          const { name, skillMd } = exportSkillMd(page.text);
          const into =
            folder ??
            (
              await vscode.window.showOpenDialog({
                canSelectFolders: true,
                canSelectFiles: false,
                canSelectMany: false,
                openLabel: 'Export here',
                title: `Export ${name} as ${SKILL_FILE}`,
              })
            )?.[0];
          if (!into) return undefined;
          const dir = vscode.Uri.joinPath(into, name);
          await vscode.workspace.fs.createDirectory(dir);
          const file = vscode.Uri.joinPath(dir, SKILL_FILE);
          await vscode.workspace.fs.writeFile(file, Buffer.from(skillMd, 'utf8'));
          if (!folder)
            void vscode.window
              .showInformationMessage(`Exported ${name} to ${file.fsPath}`, 'Open')
              .then((pick) => (pick ? vscode.window.showTextDocument(file) : undefined));
          return file;
        } catch (error) {
          void vscode.window.showErrorMessage(
            `Could not export the skill: ${describeError(error)}`,
          );
          return undefined;
        }
      },
    ),
  );
}
