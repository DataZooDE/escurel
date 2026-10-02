import type { AdminState } from '../auth/adminState';
import type { LineageNode, Skill } from '../client/types';
import { runControls } from '../runs/controls';
import { actionLabel } from '../shared/page';
import { pageSlug } from '../shared/pageId';
import type {
  ActionView,
  InspectorActions,
  InspectorView,
  ThreadNode,
  ThreadView,
  ThreadWebviewToHost,
} from '../shared/protocol';

/** Derives a skill name from an instance page id. Pure (no vscode import). */
export function skillFromPageId(pageId: string): string | undefined {
  const clean = pageId.replace(/^markdown\//, '');
  const parts = clean.split('/').filter(Boolean);
  if (parts[0] === 'instances') {
    if (parts.length >= 3) {
      return parts[1];
    }
    const file = parts.at(-1)?.replace(/\.md$/, '') ?? '';
    const sep = file.indexOf('__');
    if (sep > 0) {
      return file.slice(0, sep);
    }
  }
  return undefined;
}

export interface InspectorActionContext {
  admin?: AdminState;
  skills?: readonly Skill[];
  lineageNodes?: LineageNode[] | Map<string, LineageNode>;
  rawById?: Map<string, LineageNode>;
}

export interface ThreadActionContext {
  details?: Record<string, InspectorView>;
  rawNodes?: LineageNode[] | Map<string, LineageNode>;
  admin?: AdminState;
  skills?: readonly Skill[];
  warn?: (msg: string) => void;
}

function toRawMap(
  nodes?: LineageNode[] | Map<string, LineageNode>,
): Map<string, LineageNode> | undefined {
  if (!nodes) return undefined;
  if (nodes instanceof Map) return nodes;
  return new Map(nodes.map((n) => [n.id, n]));
}

/**
 * Finds the skill that produced a run from the lineage (never guesses).
 * As used by `src/start/approvePlan.ts`: checks parent event or root event's `label_skill`.
 */
export function resolveRunSkill(
  runNode: ThreadNode,
  rawRunNode: LineageNode | undefined,
  rawById: Map<string, LineageNode> | undefined,
): string | undefined {
  const parentId = runNode.parent ?? rawRunNode?.parent;
  if (parentId && rawById) {
    const parentNode = rawById.get(parentId);
    if (typeof parentNode?.label_skill === 'string') {
      return parentNode.label_skill;
    }
  }

  if (rawById) {
    // Check root event node
    for (const node of rawById.values()) {
      if (node.type === 'event' && node.parent === null) {
        if (typeof node.label_skill === 'string') {
          return node.label_skill;
        }
      }
    }
  }

  if (typeof rawRunNode?.label_skill === 'string') {
    return rawRunNode.label_skill;
  }
  if (typeof (rawRunNode as unknown as { skill?: unknown })?.skill === 'string') {
    return (rawRunNode as unknown as { skill: string }).skill;
  }

  return undefined;
}

/**
 * Builds the `InspectorActions` for a node in the thread inspector.
 * - Instance node (`target.open === 'page'`): Skill split buttons with page-as-UI derived labels.
 * - Run node: Cancel / Approve / Retry / Requeue / Fix skill based on run state and admin state.
 */
export function buildNodeActions(
  node: ThreadNode,
  rawNode: LineageNode | undefined,
  ctx: InspectorActionContext = {},
): InspectorActions | undefined {
  const rawById = ctx.rawById ?? toRawMap(ctx.lineageNodes);

  // 1. Instance node: target is a page, or draft targeting a page
  const pageId =
    node.target.open === 'page'
      ? node.target.pageId
      : node.kind === 'draft' && typeof rawNode?.target_page_id === 'string'
        ? rawNode.target_page_id
        : undefined;

  if (pageId) {
    const skillId =
      (rawNode as { skill?: string })?.skill ?? rawNode?.label_skill ?? skillFromPageId(pageId);

    if (!skillId || !ctx.skills) {
      return undefined;
    }

    const skill = ctx.skills.find((s) => s.id === skillId);
    if (!skill || !skill.actions || skill.actions.length === 0) {
      return undefined;
    }

    const instanceTitle = node.title || pageSlug(pageId, skill.id);
    const actions: ActionView[] = skill.actions.map((actionSkill) => ({
      skill: actionSkill,
      label: actionLabel(actionSkill, instanceTitle),
    }));

    return {
      skills: {
        pageId,
        actions,
      },
    };
  }

  // 2. Run node
  if (node.kind === 'run') {
    const status = node.state ?? rawNode?.state ?? '';
    const admin = ctx.admin ?? 'unknown';
    const controls = runControls(status, admin);
    const skill = resolveRunSkill(node, rawNode, rawById);

    return {
      controls,
      ...(skill ? { skill } : {}),
      ...({ runId: node.id } as Record<string, unknown>),
    };
  }

  return undefined;
}

/**
 * Resolves a thread webview message to an extension command and arguments.
 * Host strictly validates the message against the loaded thread (never trusts webview).
 * Returns undefined if forged or refused.
 */
export function resolveThreadAction(
  view: ThreadView,
  message: ThreadWebviewToHost,
  ctxOrDetails?: ThreadActionContext | Record<string, InspectorView>,
): { command: string; args: unknown[] } | undefined {
  const ctx: ThreadActionContext =
    ctxOrDetails && !('details' in ctxOrDetails) && !('rawNodes' in ctxOrDetails)
      ? { details: ctxOrDetails as Record<string, InspectorView> }
      : ((ctxOrDetails as ThreadActionContext) ?? {});

  const details = ctx.details;
  const rawById = toRawMap(ctx.rawNodes);

  // Helper to get actions for a node
  const getActionsForNode = (node: ThreadNode): InspectorActions | undefined => {
    if (details?.[node.id]?.actions) {
      return details[node.id]?.actions;
    }
    const rawNode = rawById?.get(node.id);
    return buildNodeActions(node, rawNode, {
      admin: ctx.admin,
      skills: ctx.skills,
      rawById,
    });
  };

  if (message.type === 'start-skill') {
    // start-skill {skill,pageId,mode}: allowed only if pageId is the page target of a node
    // in the loaded thread AND skill is one of the actions that node offers.
    const instanceNode = view.nodes.find((n) => {
      if (n.target.open === 'page' && n.target.pageId === message.pageId) {
        return true;
      }
      const raw = rawById?.get(n.id);
      return typeof raw?.target_page_id === 'string' && raw.target_page_id === message.pageId;
    });
    if (!instanceNode) {
      ctx.warn?.(`thread: refused start-skill: page ${message.pageId} is not in thread`);
      return undefined;
    }

    const actions = getActionsForNode(instanceNode);
    let offersSkill = actions?.skills?.actions.some((a) => a.skill === message.skill);
    if (!offersSkill && ctx.skills && message.pageId) {
      const pageSkillName = skillFromPageId(message.pageId);
      const pageSkill = ctx.skills.find((s) => s.id === pageSkillName);
      if (pageSkill?.actions?.includes(message.skill)) {
        offersSkill = true;
      }
    }

    if (!offersSkill) {
      ctx.warn?.(
        `thread: refused start-skill: node ${instanceNode.id} does not offer skill ${message.skill}`,
      );
      return undefined;
    }

    return {
      command: 'escurel.startSkill',
      args: [
        {
          skill: message.skill,
          pageId: message.pageId,
          mode: message.mode,
        },
      ],
    };
  }

  if (message.type === 'run-control') {
    // run-control {action, runId}: runId must be a RUN node in the loaded thread and
    // action one of the controls that run currently offers (and enabled).
    if (!message.runId) {
      ctx.warn?.('thread: refused run-control: missing runId');
      return undefined;
    }

    const runNode = view.nodes.find((n) => n.id === message.runId && n.kind === 'run');
    if (!runNode) {
      ctx.warn?.(`thread: refused run-control: run ${message.runId} is not in thread`);
      return undefined;
    }

    const actions = getActionsForNode(runNode);
    const controls = actions?.controls ?? runControls(runNode.state ?? '', ctx.admin ?? 'unknown');
    const offered = controls.find((c) => c.action === message.action);
    if (!offered || !offered.enabled) {
      ctx.warn?.(
        `thread: refused run-control: run ${runNode.id} does not offer enabled action ${message.action}`,
      );
      return undefined;
    }

    switch (message.action) {
      case 'cancel':
        return {
          command: 'escurel.cancelRun',
          args: [{ runId: runNode.id }],
        };

      case 'retry':
        return {
          command: 'escurel.retryRun',
          args: [{ runId: runNode.id }],
        };

      case 'requeue': {
        // Derived BY THE HOST from the run node's parent event in the thread
        // (the webview's eventId is ignored).
        const parentEventId = runNode.parent ?? rawById?.get(runNode.id)?.parent;
        if (!parentEventId) {
          ctx.warn?.(`thread: refused requeue: run ${runNode.id} has no parent event`);
          return undefined;
        }
        return {
          command: 'escurel.requeue',
          args: [{ eventId: parentEventId }],
        };
      }

      case 'approve': {
        // approve -> escurel.approvePlan {runId, skill, pageId} with skill and page resolved from thread
        const rawRun = rawById?.get(runNode.id);
        const skill = actions?.skill ?? resolveRunSkill(runNode, rawRun, rawById);
        const pageId =
          (rawRun as { target_page_id?: string })?.target_page_id ??
          (rawRun as { produced_instance?: string })?.produced_instance ??
          (rawById?.get(runNode.parent ?? '') as { instance_page_id?: string })?.instance_page_id;

        if (!skill || !pageId) {
          ctx.warn?.(
            `thread: refused approve: could not resolve skill or pageId for run ${runNode.id}`,
          );
          return undefined;
        }

        return {
          command: 'escurel.approvePlan',
          args: [{ runId: runNode.id, skill, pageId }],
        };
      }

      case 'fix-skill': {
        const rawRun = rawById?.get(runNode.id);
        const skill = actions?.skill ?? resolveRunSkill(runNode, rawRun, rawById);
        if (!skill) {
          ctx.warn?.(`thread: refused fix-skill: no skill resolved for run ${runNode.id}`);
          return undefined;
        }
        return {
          command: 'escurel.viewSkill',
          args: [skill],
        };
      }

      default:
        return undefined;
    }
  }

  if (message.type === 'view-skill') {
    // view-skill {skill}: the skill must be one the thread offers (an action's skill or a run's skill).
    const offeredSkills = new Set<string>();

    for (const node of view.nodes) {
      const actions = getActionsForNode(node);
      if (actions?.skills?.actions) {
        for (const act of actions.skills.actions) {
          offeredSkills.add(act.skill);
        }
      }
      if (actions?.skill) {
        offeredSkills.add(actions.skill);
      }
    }

    if (!offeredSkills.has(message.skill)) {
      ctx.warn?.(`thread: refused view-skill: skill ${message.skill} is not offered by thread`);
      return undefined;
    }

    return {
      command: 'escurel.viewSkill',
      args: [message.skill],
    };
  }

  return undefined;
}
