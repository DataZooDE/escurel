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
      try {
        const request = controlRequest(action, arg);
        const event = await services.client.captureEvent(buildControlEvent(request));
        void vscode.window.showInformationMessage(`Asked the runner to ${label[action]} ...`);
        await vscode.window.withProgress(
          {
            location: vscode.ProgressLocation.Notification,
            title: `Waiting for runner to ${label[action]}`,
            cancellable: true,
          },
          async (_progress, cancellation) => {
            const deadline = Date.now() + 30_000;
            while (!cancellation.isCancellationRequested && Date.now() < deadline) {
              const result = await findControlResult(
                (cursor) =>
                  services.client.listEvents({
                    label_skill: 'escurel:run-control-result',
                    newest_first: true,
                    include_system: true,
                    limit: 50,
                    ...(cursor ? { cursor } : {}),
                  }),
                {
                  eventId: event.event_id,
                  action,
                  ...('runId' in request && request.runId ? { runId: request.runId } : {}),
                },
              );
              if (result) {
                void vscode.window.showInformationMessage(describeOutcome(result));
                return;
              }
              await new Promise<void>((resolve) => {
                const timer = setTimeout(() => {
                  subscription.dispose();
                  resolve();
                }, 500);
                const subscription = cancellation.onCancellationRequested(() => {
                  clearTimeout(timer);
                  subscription.dispose();
                  resolve();
                });
              });
            }
            if (!cancellation.isCancellationRequested)
              void vscode.window.showInformationMessage(
                'The runner has not answered yet. It acts on requests as it polls.',
              );
          },
        );
      } catch (error) {
        void vscode.window.showErrorMessage(describeControlRefusal(error));
      }
    });
  context.subscriptions.push(
    register('escurel.cancelRun', 'cancel'),
    register('escurel.retryRun', 'retry'),
    register('escurel.requeue', 'requeue'),
    register('escurel.pauseDispatch', 'pause'),
    register('escurel.resumeDispatch', 'resume'),
  );
}
