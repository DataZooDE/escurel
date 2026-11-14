import * as vscode from 'vscode';
import { describeError } from '../errors';
import type { Services } from '../services';
import {
  CHAT_CHANNEL_SKILL,
  groupProblem,
  idProblem,
  mappingPage,
  mappingsOfTarget,
  type MappingInput,
} from './chatChannelModel';
import { collectInstances } from './scenariosModel';

/**
 * "Map Teams conversation…" (#830, admin): connect a Teams group chat or channel to an escurel group
 * by writing a `chat-channel` page. A conversation already mapped is offered for editing instead of
 * getting a second, conflicting page.
 */
export function registerChatChannelCommands(
  context: vscode.ExtensionContext,
  services: Services,
  onMapped: () => void,
): void {
  context.subscriptions.push(
    vscode.commands.registerCommand('escurel.chatChannel.map', async () => {
      if (await mapConversation(services)) onMapped();
    }),
  );
}

async function ask(prompt: string, check: (v: string) => string | undefined, placeHolder?: string) {
  return vscode.window.showInputBox({
    prompt,
    placeHolder,
    ignoreFocusOut: true,
    validateInput: (v) => check(v) ?? null,
  });
}

async function mapConversation(services: Services): Promise<boolean> {
  const kind = await vscode.window.showQuickPick(
    [
      { label: 'Group chat', description: 'by its conversation id', target: 'group' as const },
      {
        label: 'Channel',
        description: 'by team and channel id (every thread)',
        target: 'channel' as const,
      },
    ],
    { title: 'Map Teams conversation', placeHolder: 'What should act as an escurel group?' },
  );
  if (!kind) return false;
  let input: MappingInput;
  if (kind.target === 'group') {
    const conversationId = await ask(
      "The group chat's conversation id",
      (v) => idProblem('conversation id', v),
      '19:…@thread.v2',
    );
    if (conversationId === undefined) return false;
    const group = await ask('The escurel group it acts as', groupProblem, 'sales');
    if (group === undefined) return false;
    input = { kind: 'group', conversationId, group };
  } else {
    const teamId = await ask('The team id', (v) => idProblem('team id', v), '19:…@thread.tacv2');
    if (teamId === undefined) return false;
    const channelId = await ask(
      'The channel id',
      (v) => idProblem('channel id', v),
      '19:…@thread.tacv2',
    );
    if (channelId === undefined) return false;
    const group = await ask('The escurel group it acts as', groupProblem, 'sales');
    if (group === undefined) return false;
    input = { kind: 'channel', teamId, channelId, group };
  }
  const page = mappingPage(input);
  try {
    const existing = mappingsOfTarget(
      input,
      await collectInstances(
        services.client.listInstances({ skill_id: CHAT_CHANNEL_SKILL, limit: 500 }),
        5000,
      ),
    );
    if (existing.length) {
      const open = 'Open the mapping';
      const pick = await vscode.window.showWarningMessage(
        existing.length > 1
          ? 'This conversation already has more than one chat-channel page; the bot refuses it ' +
              'until one is left. Open them and delete all but one.'
          : 'This conversation is already mapped. Change the group on its chat-channel page.',
        open,
      );
      if (pick === open)
        for (const m of existing)
          await vscode.commands.executeCommand('escurel.openPage', m.page_id);
      return false;
    }
    const res = await services.client.updatePage({
      page_id: page.pageId,
      content: page.content,
      base_sha256: '',
    });
    const issues =
      (res as { ok: boolean; issues?: { code: string; message: string }[] }).issues ?? [];
    if (!(res as { ok: boolean }).ok) {
      const why = issues.map((i) => `${i.code}: ${i.message}`).join('; ') || 'refused';
      void vscode.window.showErrorMessage(
        why.includes('conflict')
          ? 'This conversation is already mapped; open its chat-channel page to change it.'
          : `Could not map the conversation: ${why}`,
      );
      return false;
    }
  } catch (error) {
    void vscode.window.showErrorMessage(`Could not map the conversation: ${describeError(error)}`);
    return false;
  }
  void vscode.window.showInformationMessage(
    `Mapped: @mentions there now act as group:${input.group.trim()}, for members of ${input.group.trim()} only.`,
  );
  await vscode.commands.executeCommand('escurel.openPage', page.pageId);
  return true;
}
