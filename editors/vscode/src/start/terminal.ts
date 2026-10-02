import * as vscode from 'vscode';
import { describeError } from '../errors';
import type { Services } from '../services';
import {
  buildTerminalSpec,
  canStartInTerminal,
  newSpanId,
  newTraceId,
  parseTerminalArgs,
} from './terminalSpec';

export function registerStartInTerminal(
  context: vscode.ExtensionContext,
  services: Services,
): void {
  context.subscriptions.push(
    vscode.commands.registerCommand('escurel.startInTerminal', async (raw: unknown) => {
      const arg = parseTerminalArgs(raw);
      if (!arg) {
        void vscode.window.showErrorMessage(
          'Cannot start in a terminal without a skill and a page.',
        );
        return;
      }
      const command =
        vscode.workspace.getConfiguration('escurel').get<string>('shellHarness') ?? '';
      const guard = canStartInTerminal({ trusted: vscode.workspace.isTrusted, command });
      if (!guard.ok) {
        void vscode.window.showInformationMessage(guard.reason);
        return;
      }

      let token: string | undefined;
      try {
        const traceId = newTraceId();
        const mint = await services.client.mintAgentToken({
          skill: arg.skill,
          target_page_id: arg.pageId,
          ...(arg.rootEventId ? { root_event_id: arg.rootEventId } : {}),
          trace_id: traceId,
        });
        token = mint.token;
        const spec = buildTerminalSpec({
          skill: arg.skill,
          pageId: arg.pageId,
          gatewayUrl: services.gatewayUrl,
          command,
          mint,
          traceId,
          spanId: newSpanId(),
        });
        const terminal = vscode.window.createTerminal({ name: spec.name, env: spec.env });
        terminal.show();
        terminal.sendText(spec.command);
        // A run minted without a root event has NO thread: the gateway returns a lineage only for
        // a root that is a real, readable event, and a synthetic root reads as absent. Its run
        // detail, built from `list_events{run_id}`, is where it shows. Given a real root (started
        // from a thread), the run hangs under it and the thread is the place to look.
        if (arg.rootEventId) {
          await vscode.commands.executeCommand('escurel.openThread', arg.rootEventId);
        } else {
          await vscode.commands.executeCommand('escurel.openRun', mint.run_id);
        }
        return { runId: mint.run_id, rootEventId: mint.root_event_id };
      } catch (error) {
        // An error after mint may carry the bearer. Never surface it in a notice.
        const message = describeError(error);
        void vscode.window.showErrorMessage(
          `Could not start in terminal — ${token ? message.split(token).join('[redacted]') : message}`,
        );
        return;
      }
    }),
  );
}
