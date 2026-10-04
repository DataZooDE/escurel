import * as vscode from 'vscode';
import type { Services } from '../services';
import { describeError } from '../errors';
import { pageSkill } from '../shared/pageId';
import { findThreadStrip } from '../shared/threadStrip';
import { nodeRefs, skillThreadItems } from './nodeRefs';
import { noThreadMessage } from './noThreadWording';

/**
 * Navigation from a row of any tree to the other places it belongs: its thread, its run, its skill, and
 * (for a skill) the threads it started. Each is a thin wrapper over the command that already owns that
 * surface, so every route lands in the same place.
 */
export function registerNodeCommands(context: vscode.ExtensionContext, services: Services): void {
  const note = (message: string) => void vscode.window.setStatusBarMessage(message, 6000);

  /** The thread and run that last wrote a page, read from the page's own events. */
  const strip = (pageId: string) =>
    findThreadStrip((cursor) =>
      services.client.listEvents({
        instance_page_id: pageId,
        include_system: true,
        newest_first: true,
        limit: 50,
        ...(cursor ? { cursor } : {}),
      }),
    );

  /** Said when a record has no thread or run: what is true for ITS source, and a way to the runs that read it. */
  const noThread = async (pageId: string | undefined): Promise<void> => {
    let backend: string | undefined;
    const skill = pageId ? pageSkill(pageId) : undefined;
    if (skill) {
      try {
        backend = (await services.client.listSkills()).find((s) => s.id === skill)?.backend.kind;
      } catch {
        // The words are a courtesy: without the skill list the general sentence still says what to do.
      }
    }
    const show = 'Runs for this record';
    const pick = await vscode.window.showInformationMessage(
      noThreadMessage(backend),
      ...(pageId ? [show] : []),
    );
    if (pick === show && pageId)
      await vscode.commands.executeCommand('escurel.runs.forPage', pageId);
  };

  context.subscriptions.push(
    vscode.commands.registerCommand('escurel.openNodeThread', async (arg: unknown) => {
      const refs = nodeRefs(arg);
      let root = refs.rootEventId;
      if (!root && refs.pageId) root = (await strip(refs.pageId))?.rootEventId;
      if (!root) return void (await noThread(refs.pageId));
      await vscode.commands.executeCommand('escurel.openThread', root);
    }),
    vscode.commands.registerCommand('escurel.openNodeRun', async (arg: unknown) => {
      const refs = nodeRefs(arg);
      let run = refs.runId;
      if (!run && refs.pageId) run = (await strip(refs.pageId))?.runId;
      if (!run) return void (await noThread(refs.pageId));
      await vscode.commands.executeCommand('escurel.openRun', run);
    }),
    vscode.commands.registerCommand('escurel.viewNodeSkill', async (arg: unknown) => {
      const { skill } = nodeRefs(arg);
      if (!skill)
        return void vscode.window.showInformationMessage('This row does not belong to a skill.');
      await vscode.commands.executeCommand('escurel.viewSkill', skill);
    }),
    vscode.commands.registerCommand('escurel.showSkillThreads', async (arg: unknown) => {
      const { skill } = nodeRefs(arg);
      if (!skill) return;
      const role =
        typeof arg === 'object' && arg !== null
          ? ((arg as { skill?: { role?: string } }).skill?.role ?? undefined)
          : undefined;
      if (role === 'report') {
        return void vscode.window.showInformationMessage(
          `${skill} is a report: it draws the data of a record (for example a chart) and is never run, so it has no threads. Open a record that uses it instead.`,
        );
      }
      try {
        const page = await services.client.listEvents({
          label_skill: skill,
          newest_first: true,
          limit: 100,
        });
        const items = skillThreadItems(page.events);
        if (items.length === 0) {
          return void vscode.window.showInformationMessage(
            `Nothing has been filed under ${skill} yet.`,
          );
        }
        const picked = await vscode.window.showQuickPick(items, {
          title: `Threads started by ${skill}`,
          placeHolder: 'Open a thread',
          matchOnDescription: true,
        });
        if (picked) await vscode.commands.executeCommand('escurel.openThread', picked.rootEventId);
      } catch (e) {
        void vscode.window.showErrorMessage(describeError(e));
      } finally {
        note('');
      }
    }),
  );
}
