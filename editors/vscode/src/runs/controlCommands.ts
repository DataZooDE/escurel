import { inFlight, pollControlResult } from './controlWait';
import * as vscode from 'vscode';
import type { Services } from '../services';
import { buildControlEvent, type ControlRequest } from './controls';
import { describeControlRefusal, describeOutcome, findControlResult } from './controlResult';
import { registerAdminContext } from './adminContext';

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

const label: Record<Action, string> = {
  cancel: 'cancel',
  retry: 'retry',
  requeue: 'requeue',
  pause: 'pause dispatch',
  resume: 'resume dispatch',
};

export function registerControlCommands(
  context: vscode.ExtensionContext,
  services: Services,
): void {
  registerAdminContext(context, services);
  const once = inFlight();
  const register = (command: string, action: Action) =>
    vscode.commands.registerCommand(command, async (arg?: Argument) => {
      if (action === 'pause') {
        const answer = await vscode.window.showWarningMessage(
          'Pause dispatch for the whole tenant?',
          { modal: true },
          'Pause dispatch',
        );
        if (answer !== 'Pause dispatch') return;
      }
      let request: ReturnType<typeof controlRequest>;
      try {
        request = controlRequest(action, arg);
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
            location: vscode.ProgressLocation.Notification,
            title: `Waiting for runner to ${label[action]}`,
            cancellable: true,
          },
          async (_progress, cancellation) => {
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
              cancelled: () => cancellation.isCancellationRequested,
            });
            if (outcome.kind === 'result') {
              void vscode.window.showInformationMessage(describeOutcome(outcome.result));
            } else if (outcome.kind === 'timeout') {
              void vscode.window.showInformationMessage(
                outcome.lookupFailed
                  ? 'The request was sent, but the answer could not be read. Check the Runner view.'
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
