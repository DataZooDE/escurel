import * as vscode from 'vscode';
import { describeError } from '../errors';
import { rowSourceOf } from '../shared/rowSource';
import { buildProposal, describeCurrent, parseProposedValue } from '../shared/writeBack';
import type { Services } from '../services';

/**
 * `escurel.proposeWriteBack`: propose a change to a WRITABLE column of a row of a remote source.
 *
 * Nothing reaches the source from here. The change is held as a draft that carries a `write_back`
 * block; a reviewer promotes it from Awaiting You, and only then does the gateway change the source
 * (after checking the row is still as it was read). The command re-reads the row so the proposal is
 * based on what is there NOW, and refuses a column the source does not say is writable.
 */
export function registerProposeWriteBack(services: Services): vscode.Disposable {
  return vscode.commands.registerCommand(
    'escurel.proposeWriteBack',
    async (arg: { pageId?: unknown; field?: unknown } | undefined) => {
      if (typeof arg?.pageId !== 'string' || typeof arg.field !== 'string') return;
      const { pageId, field } = arg;
      try {
        const client = services.client;
        const page = await client.expand({ page_id: pageId });
        const source = rowSourceOf((page as { backend_projection?: unknown }).backend_projection);
        if (!page.page || !source?.etag || !source.writableColumns?.includes(field)) {
          void vscode.window.showWarningMessage(
            `${field} cannot be changed in the source from here.`,
          );
          return;
        }
        const current = page.frontmatter[field];
        const raw = await vscode.window.showInputBox({
          title: `Change ${field} in the source`,
          prompt: `Currently ${describeCurrent(current)}. A reviewer approves the change before the source is touched.`,
          validateInput: (v) => {
            const r = parseProposedValue(v, current);
            return r.ok ? undefined : r.error;
          },
        });
        if (raw === undefined) return;
        const parsed = parseProposedValue(raw, current);
        if (!parsed.ok) return;
        const notes = await vscode.window.showInputBox({
          title: 'Note for the reviewer (optional)',
          prompt: "Why this change? It is kept as the row's notes when the change is approved.",
        });
        if (notes === undefined) return;
        await client.createDraft({
          target_page_id: pageId,
          content: buildProposal({
            pageId,
            skill: page.page.skill,
            field,
            value: parsed.value,
            baseEtag: source.etag,
            notes: notes.trim(),
          }),
        });
        void vscode.window.showInformationMessage(
          `Proposed: ${field} to ${describeCurrent(parsed.value)}. A reviewer approves it from Awaiting You.`,
        );
        void vscode.commands.executeCommand('escurel.refresh');
      } catch (e) {
        void vscode.window.showErrorMessage(`Could not propose the change: ${describeError(e)}`);
      }
    },
  );
}
