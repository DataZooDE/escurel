import * as vscode from 'vscode';
import { createHash } from 'node:crypto';
import type { Services } from '../services';
import { readPageMarkdown } from '../fs/read';
import { describeError } from '../errors';
import { preparedV2Draft, v2TrainingStarter } from './problemImport';
import {
  parsePreparationReceipt, preparationEvent, preparationReceiptId,
  trainingSourcePage, trainingSourcePayload,
} from './sourceImport';

export function registerPrepareEvolveTrainingSource(
  context: vscode.ExtensionContext, services: Services,
): vscode.Disposable {
  type PendingSource = { fileUri: string; fileSha256: string; owner: string; pageSha256: string };
  const pendingKey = 'evolve.pendingTrainingSources';
  const pendingSources = () => context.workspaceState.get<Record<string, PendingSource>>(pendingKey, {});
  const openPreparedDraft = async (eventId: string, result: Record<string, unknown>, pageSha256: string) => {
    const pending = pendingSources()[eventId];
    if (!pending || pending.pageSha256 !== pageSha256) return false;
    if (await services.subject() !== pending.owner)
      throw new Error('Sign in as the source owner to reopen its local draft.');
    const bytes = await vscode.workspace.fs.readFile(vscode.Uri.parse(pending.fileUri));
    if (createHash('sha256').update(bytes).digest('hex') !== pending.fileSha256)
      throw new Error('The selected training JSON changed after preparation. Restore that file to reopen its draft.');
    const input: unknown = JSON.parse(new TextDecoder().decode(bytes));
    const source = trainingSourcePayload(input);
    const sourceId = result.training_source_id;
    const digest = result.normalized_sha256;
    if (typeof sourceId !== 'string' || typeof digest !== 'string' || !/^[a-f0-9]{64}$/.test(digest))
      throw new Error('The preparation receipt lacks a valid source ID or digest.');
    const document = await vscode.workspace.openTextDocument({
      language: 'json', content: JSON.stringify(preparedV2Draft(input, source, sourceId, digest), null, 2) + '\n',
    });
    await vscode.window.showTextDocument(document, { preview: false });
    return true;
  };
  const check = vscode.commands.registerCommand('escurel.checkEvolveTrainingSource', async (knownEventId?: string) => {
    try {
      const eventId = knownEventId ?? await vscode.window.showInputBox({ prompt: 'Escurel source preparation event ID' });
      if (!eventId) return;
      const root = (await services.client.listEvents({ event_id: eventId })).events
        .find((candidate) => candidate.event_id === eventId && candidate.label_skill === 'evolve_prepare_source');
      const manual = root?.provenance?.manual as Record<string, unknown> | undefined;
      const pageSha = manual?.target_page_sha256;
      if (typeof pageSha !== 'string') throw new Error('The source preparation event is unavailable.');
      const receipts = await services.client.listEvents({ event_id: preparationReceiptId(eventId), include_system: true });
      const result = receipts.events.map((receipt) => parsePreparationReceipt(receipt, eventId, pageSha))
        .find((value) => value !== undefined);
      if (!result) {
        void vscode.window.showInformationMessage(`Preparation ${eventId} is still pending. Check again later.`);
      } else if (result.prepared === true) {
        const canReopen = pendingSources()[eventId]?.pageSha256 === pageSha;
        const action = await vscode.window.showInformationMessage(
          `Source ${String(result.training_source_id)} prepared. Digest: ${String(result.normalized_sha256)}. Register a matching private holdout, then import a completed V2 spec.`,
          ...(canReopen ? ['Open prepared V2 draft'] : []),
        );
        if (action === 'Open prepared V2 draft') await openPreparedDraft(eventId, result, pageSha);
      } else {
        void vscode.window.showErrorMessage(`Source preparation failed: ${String(result.issue ?? 'inspect the event')}`);
      }
    } catch (error) {
      void vscode.window.showErrorMessage(`Evolve preparation check failed: ${describeError(error)}`);
    }
  });
  const command = vscode.commands.registerCommand('escurel.prepareEvolveTrainingSource', async () => {
    try {
      const owner = await services.subject();
      if (!owner) throw new Error('Sign in before preparing an owner-private source.');
      const choice = await vscode.window.showQuickPick([
        { label: 'Open starter source JSON', description: 'Edit and save it locally first' },
        { label: 'Prepare a local JSON file', description: 'Eight-field source or full V2 training spec' },
      ], { placeHolder: 'Prepare an Anofox Evolve V2 training source' });
      if (!choice) return;
      if (choice.label === 'Open starter source JSON') {
        const document = await vscode.workspace.openTextDocument({
          language: 'json',
          content: JSON.stringify(trainingSourcePayload(v2TrainingStarter), null, 2) + '\n',
        });
        await vscode.window.showTextDocument(document, { preview: false });
        return;
      }
      const files = await vscode.window.showOpenDialog({
        canSelectMany: false, openLabel: 'Choose training JSON', filters: { JSON: ['json'] },
      });
      const file = files?.[0];
      if (!file) return;
      const fileBytes = await vscode.workspace.fs.readFile(file);
      const input: unknown = JSON.parse(new TextDecoder().decode(fileBytes));
      const id = await vscode.window.showInputBox({
        prompt: 'Private source page ID (lowercase letters, numbers, underscores, hyphens)',
        ignoreFocusOut: true,
      });
      if (id === undefined) return;
      const page = trainingSourcePage({ id: id.trim(), owner, value: input });
      const source = page.payload;
      const confirmation = await vscode.window.showWarningMessage(
        `Prepare ${String(source.skus instanceof Array ? source.skus.length : '?')} SKUs `
        + `from ${String(source.training_start)} to ${String(source.training_end)}? `
        + 'You attest true demand and opening inventory as of training start. '
        + 'The owner-private page stores these training rows; Evolve seals their submitted bytes.',
        { modal: true }, 'Prepare source',
      );
      if (confirmation !== 'Prepare source') return;
      const client = services.client;
      const existing = await readPageMarkdown(client, page.pageId);
      if (existing && (existing.skill !== 'evolve_training_source' || existing.degraded
          || !existing.sha256 || existing.frontmatter.owner_subject !== owner
          || existing.lastWrittenBy !== owner)) {
        throw new Error('This source page is unavailable for an owner-authored revision.');
      }
      const validation = await client.validate({ content: page.content, as_page_id: page.pageId });
      if (!validation.ok || validation.issues.some((issue) => issue.severity === 'error'))
        throw new Error(`Escurel rejected this source page: ${validation.issues.map((issue) => issue.message).join('; ')}`);
      const written = await client.updatePage({
        page_id: page.pageId, content: page.content, base_sha256: existing?.sha256 ?? '',
      });
      if (!written.ok)
        throw new Error(`Escurel rejected this source page: ${written.issues.map((issue) => issue.message).join('; ')}`);
      const stored = await readPageMarkdown(client, page.pageId);
      if (!stored?.sha256 || stored.text !== page.content || stored.lastWrittenBy !== owner)
        throw new Error('Source page changed during preparation. Review it and retry.');
      services.onDidChangeEmit();
      const event = await client.captureEvent(preparationEvent(page.pageId, stored.sha256));
      await context.workspaceState.update(pendingKey, {
        ...pendingSources(),
        [event.event_id]: {
          fileUri: file.toString(), fileSha256: createHash('sha256').update(fileBytes).digest('hex'),
          owner, pageSha256: stored.sha256,
        },
      });
      const receiptId = preparationReceiptId(event.event_id);
      let result: Record<string, unknown> | undefined;
      for (let attempt = 0; attempt < 80; attempt++) {
        const listed = await client.listEvents({ event_id: receiptId, include_system: true });
        result = listed.events.map((candidate) => parsePreparationReceipt(
          candidate, event.event_id, stored.sha256!,
        )).find((value) => value !== undefined);
        if (result) break;
        await new Promise<void>((resolve) => setTimeout(resolve, 750));
      }
      if (!result) {
        const action = await vscode.window.showWarningMessage(
          `Source preparation is still pending in Escurel event ${event.event_id}.`, 'Check preparation',
        );
        if (action === 'Check preparation') await vscode.commands.executeCommand('escurel.checkEvolveTrainingSource', event.event_id);
        return;
      }
      if (result.prepared !== true)
        throw new Error(`Source preparation was rejected: ${String(result.issue ?? 'inspect the request thread')}`);
      await openPreparedDraft(event.event_id, result, stored.sha256);
      void vscode.window.showInformationMessage(
        `Source ${String(result.training_source_id)} prepared. The opened V2 draft needs policy SQL, service targets, costs, windows, and budget. Save it, register a matching private holdout, then import the completed spec.`,
      );
    } catch (error) {
      void vscode.window.showErrorMessage(`Evolve source preparation failed: ${describeError(error)}`);
    }
  });
  context.subscriptions.push(command, check);
  return command;
}
