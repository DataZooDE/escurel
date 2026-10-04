import { notify } from '../commands/notify';
import { inFlight, pollControlResult } from './controlWait';
import * as vscode from 'vscode';
import type { Services } from '../services';
import { buildControlEvent, type ControlRequest } from './controls';
import { describeControlRefusal, describeOutcome, findControlResult } from './controlResult';
import { registerAdminContext } from './adminContext';
import { asksHere, confirmationFor, outcomeChannel, progressTitle } from './controlWording';

type Action = ControlRequest['action'];
export type Argument =
  | string
  | { runId?: string | undefined; eventId?: string | undefined; id?: string | undefined }
  | undefined;

/** What a command's argument means: a bare id, or a tree row / message carrying `runId` / `eventId`. */
export function controlRequest(action: Action, arg: Argument): ControlRequest {
  const id = typeof arg === 'string' ? arg : arg?.id;
  if (action === 'cancel' || action === 'retry')
    return { action, runId: typeof arg === 'string' ? arg : (arg?.runId ?? id) };
  if (action === 'requeue')
    return { action, eventId: typeof arg === 'string' ? arg : (arg?.eventId ?? id) };
  return { action };
}

export function registerControlCommands(
  context: vscode.ExtensionContext,
  services: Services,
): void {
  registerAdminContext(context, services);
  const once = inFlight();
  const register = (command: string, action: Action) =>
    vscode.commands.registerCommand(command, async (arg?: Argument) => {
      const confirmation = asksHere(arg) ? confirmationFor(action) : undefined;
      if (confirmation) {
        const answer = await vscode.window.showWarningMessage(
          confirmation.message,
          { modal: true, detail: confirmation.detail },
          confirmation.button,
        );
        if (answer !== confirmation.button) return;
      }
      let request: ReturnType<typeof controlRequest>;
      try {
        request = controlRequest(action, arg);
        // From the palette there is no run to act on: say where to pick one.
        if ((action === 'cancel' || action === 'retry') && !('runId' in request && request.runId)) {
          void vscode.window.showInformationMessage(
            `Select a run in the Runs view or in a thread, then use its menu to ${action} it.`,
          );
          return;
        }
        if (action === 'requeue' && !('eventId' in request && request.eventId)) {
          void vscode.window.showInformationMessage(
            'Select a dead letter in the Runs view, then use its menu to requeue it.',
          );
          return;
        }
      } catch (error) {
        void vscode.window.showErrorMessage(describeControlRefusal(error));
        return;
      }
      const key = `${action}:${'runId' in request ? (request.runId ?? '') : ''}:${'eventId' in request ? (request.eventId ?? '') : ''}`;
      await once(key, async () => {
        let eventId: string;
        try {
          eventId = (await services.client.captureEvent(buildControlEvent(request))).event_id;
        } catch (error) {
          // Only HERE is a refusal real: the gateway did not take the request.
          void vscode.window.showErrorMessage(describeControlRefusal(error));
          return;
        }
        await vscode.window.withProgress(
          {
            // The status bar, not a toast: one line while the runner picks it up.
            location: vscode.ProgressLocation.Window,
            title: progressTitle(action),
          },
          async () => {
            const outcome = await pollControlResult({
              find: () =>
                findControlResult(
                  (cursor) =>
                    services.client.listEvents({
                      label_skill: 'escurel:run-control-result',
                      newest_first: true,
                      include_system: true,
                      limit: 50,
                      ...(cursor ? { cursor } : {}),
                    }),
                  {
                    eventId,
                    action,
                    ...('runId' in request && request.runId ? { runId: request.runId } : {}),
                  },
                ),
              now: Date.now,
              sleep: (ms) => new Promise<void>((resolve) => setTimeout(resolve, ms)),
              timeoutMs: 30_000,
              intervalMs: 500,
              cancelled: () => false,
            });
            if (outcome.kind === 'result') {
              const text = describeOutcome(outcome.result);
              if (outcomeChannel(outcome.result.outcome) === 'status') {
                vscode.window.setStatusBarMessage(text, 8000);
              } else {
                // The result may name a run (the one acted on, or the new one a retry started): offer to open it.
                void notify('warning', text, [
                  {
                    kind: 'run',
                    runId: outcome.result.newRunId ?? outcome.result.runId ?? undefined,
                  },
                ]);
              }
            } else if (outcome.kind === 'timeout') {
              void vscode.window.showInformationMessage(
                outcome.lookupFailed
                  ? 'The request was sent, but the answer could not be read. Check the Runs view.'
                  : 'The runner has not answered yet. It acts on requests as it polls.',
              );
            }
          },
        );
      });
    });
  context.subscriptions.push(
    register('escurel.cancelRun', 'cancel'),
    register('escurel.retryRun', 'retry'),
    register('escurel.requeue', 'requeue'),
    register('escurel.pauseDispatch', 'pause'),
    register('escurel.resumeDispatch', 'resume'),
  );
}
