import { pageSlug } from './pageId';

/** Something a notification can be about, and so can offer to open. */
export type NoticeTarget =
  | { kind: 'page'; pageId: string | undefined }
  | { kind: 'run'; runId: string | undefined }
  | { kind: 'thread'; rootEventId: string | undefined }
  | { kind: 'skill'; skill: string | undefined };

export interface NoticeAction {
  label: string;
  command: string;
  args: unknown[];
}

const MAX_BUTTONS = 2;

function actionFor(t: NoticeTarget): NoticeAction | undefined {
  switch (t.kind) {
    case 'page':
      return t.pageId
        ? { label: `Open ${pageSlug(t.pageId)}`, command: 'escurel.openInstance', args: [t.pageId] }
        : undefined;
    case 'run':
      return t.runId
        ? { label: 'Open run', command: 'escurel.openRun', args: [t.runId] }
        : undefined;
    case 'thread':
      return t.rootEventId
        ? { label: 'Open thread', command: 'escurel.openThread', args: [t.rootEventId] }
        : undefined;
    case 'skill':
      return t.skill
        ? { label: 'View skill', command: 'escurel.viewSkill', args: [t.skill] }
        : undefined;
  }
}

/**
 * The buttons of a notification that names things: one per target, in the order given, without
 * duplicates, at most two (a toast stays small; the trees reach the rest). Pure: `notify` shows them.
 */
export function noticeActions(targets: NoticeTarget[]): NoticeAction[] {
  const out: NoticeAction[] = [];
  for (const target of targets) {
    const action = actionFor(target);
    if (!action) continue;
    if (out.some((o) => o.command === action.command && o.args[0] === action.args[0])) continue;
    out.push(action);
    if (out.length === MAX_BUTTONS) break;
  }
  return out;
}
