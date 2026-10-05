import type { AdminState } from '../auth/adminState';
import type { LineageNode, Skill } from '../client/types';
import { factsFromLineage, offeredControls, resolveControl, type RunFacts } from '../runs/runFacts';
import { skillActionViews } from '../shared/actions';
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
 * The run's facts from the thread's lineage, in the same shape the run page builds them, so both
 * surfaces offer and resolve the same controls. Nothing is guessed: a trigger event that lineage
 * pruned leaves `triggerEventId` and `skill` unknown, and the controls that need them are not
 * offered (see `visibleControls`). `ThreadNode.parent` is NOT used: the thread view hangs a node
 * whose parent was pruned off the root, which is not its trigger.
 */
function threadRunFacts(
  node: ThreadNode,
  rawNode: LineageNode | undefined,
  rawById: Map<string, LineageNode> | undefined,
  admin: AdminState,
): RunFacts {
  const byId = new Map(rawById ?? []);
  if (rawNode) byId.set(node.id, rawNode);
  return factsFromLineage(node.id, byId, admin, node.state ?? rawNode?.state ?? '');
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

    // Validation requires the displayed experiment revision and winner;
    // the thread inspector does not hold either verified page snapshot.
    const actions: ActionView[] = skillActionViews(skill.actions)
      .filter((action) => !['evolve_validate', 'evolve_publish_candidate'].includes(action.skill));
    if (actions.length === 0) return undefined;

    return {
      skills: {
        pageId,
        actions,
      },
    };
  }

  // 2. Run node
  if (node.kind === 'run') {
    const facts = threadRunFacts(node, rawNode, rawById, ctx.admin ?? 'unknown');
    return {
      controls: offeredControls(facts),
      ...(facts.skill ? { skill: facts.skill } : {}),
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
      if (message.skill !== 'evolve_validate' && message.skill !== 'evolve_publish_candidate'
          && skillActionViews(pageSkill?.actions).some((a) => a.skill === message.skill)) {
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

    // Recomputed from the host's lineage, not read back from what was sent to the webview.
    const facts = threadRunFacts(
      runNode,
      rawById?.get(runNode.id),
      rawById,
      ctx.admin ?? 'unknown',
    );
    const offered = offeredControls(facts).find((c) => c.action === message.action);
    if (!offered || !offered.enabled) {
      ctx.warn?.(
        `thread: refused run-control: run ${runNode.id} does not offer enabled action ${message.action}`,
      );
      return undefined;
    }
    // The webview's own eventId, skill or page (if it sent any) are never read.
    const resolved = resolveControl(facts, offered.action);
    if (!resolved) {
      ctx.warn?.(`thread: refused ${message.action}: run ${runNode.id} lacks what it needs`);
      return undefined;
    }
    return { command: resolved.command, args: [resolved.arg] };
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
