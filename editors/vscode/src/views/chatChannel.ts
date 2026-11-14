import * as vscode from 'vscode';
import { describeError } from '../errors';
import type { Services } from '../services';
import { groupProblem, idProblem, mappingPage, type MappingInput } from './chatChannelModel';

/**
 * "Map Teams conversation…" (#830, admin): connect a Teams group chat or channel to an escurel group
 * by writing a `chat-channel` page. Create-only: an existing mapping is edited as a page.
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
