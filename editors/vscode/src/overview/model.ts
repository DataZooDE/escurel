// The overview board's model: pure, no `vscode`. What needs a person today, what the agents are doing,
// what is open, what just finished: five small answers built from data the other views already read.
import type { OverviewItem, OverviewTile, OverviewTone, OverviewView } from '../shared/protocol';
import { cleanText } from '../shared/untrustedText';
import type { AwaitingRow } from '../views/awaitingModel';
import { awaitingDisplay } from '../views/awaitingDisplay';
import {
  groupRuns,
  insightLine,
  runDescription,
  runRowLabel,
  shortAgo,
  type RunRecord,
  type RunnerDescription,
} from '../views/runsModel';

/** A tile shows this many lines; the rest is a count, and the tile's title opens the full view. */
export const TILE_LINES = 5;

/** A command the host may run for a key it handed out. Built here, resolved by the host, never read from the webview. */
export interface OverviewAction {
  command: string;
  args: unknown[];
}

export interface OpenSkillCount {
  skill: string;
  title: string;
  /** Instances counted on the first page. */
  count: number;
  /** Whether more lie beyond that page ("50+"). */
  more: boolean;
}

export interface OverviewInputs {
  nowMs: number;
  focusOn: boolean;
  awaiting: readonly AwaitingRow[];
  runs: readonly RunRecord[];
  runner: Pick<RunnerDescription, 'text' | 'paused' | 'health'> | undefined;
  open: readonly OpenSkillCount[];
}

export interface Overview {
  view: OverviewView;
  /** Key -> what it runs. The host keeps this next to the view it sent. */
  actions: Map<string, OverviewAction>;
}

const plural = (n: number, one: string, many: string) => (n === 1 ? one : many);

export function buildOverview(input: OverviewInputs): Overview {
  const actions = new Map<string, OverviewAction>();
  const line = (
    tile: OverviewTile['id'],
    index: number,
    item: Omit<OverviewItem, 'key'>,
    action: OverviewAction,
  ): OverviewItem => {
    const key = `${tile}:${index}`;
    actions.set(key, action);
    return { key, ...item };
  };
  const capped = <T>(all: readonly T[]): { shown: T[]; more: number | undefined } => ({
    shown: all.slice(0, TILE_LINES),
    more: all.length > TILE_LINES ? all.length - TILE_LINES : undefined,
  });
  const tile = (t: Omit<OverviewTile, 'more'> & { more?: number | undefined }): OverviewTile => {
    const { more, ...rest } = t;
    return more === undefined ? rest : { ...rest, more };
  };

  // 1. Decisions: the same queue as "Awaiting you".
  const waiting = capped(input.awaiting);
  const decisions = tile({
    id: 'decisions',
    title: 'Decisions waiting',
    headline:
      input.awaiting.length === 0
        ? 'Nothing is waiting for you'
        : `${input.awaiting.length} waiting for you`,
    tone: input.awaiting.length === 0 ? 'ok' : 'attention',
    items: waiting.shown.map((row, i) => {
      const shown = awaitingDisplay(row, input.nowMs);
      return line(
        'decisions',
        i,
        { label: shown.label, detail: shown.description, tone: 'attention' },
        row.kind === 'plan'
          ? { command: 'escurel.openRun', args: [{ runId: row.runId }] }
          : { command: 'escurel.openReview', args: [row] },
      );
    }),
    more: waiting.more,
    empty: 'Nothing needs your decision right now.',
  });

  // 2. Agents: how they are, and what is running now.
  const groups = groupRuns(input.runs, input.nowMs);
  const running = capped(groups.running);
  const runnerTone: OverviewTone =
    !input.runner || input.runner.health === 'none'
      ? 'neutral'
      : input.runner.health === 'ok' && !input.runner.paused
        ? 'ok'
        : 'attention';
  const agents = tile({
    id: 'agents',
    title: 'Agent activity',
    headline: input.runner?.text ?? 'No agents have reported yet.',
    tone: runnerTone,
    items: running.shown.map((r, i) =>
      line(
        'agents',
        i,
        { label: runRowLabel(r), detail: runDescription(r, input.nowMs), tone: 'neutral' },
        { command: 'escurel.openRun', args: [{ runId: r.runId }] },
      ),
    ),
    more: running.more,
    empty: 'No agent is running right now.',
  });

  // 3. Attention: failures nobody has retried since.
  const failed = capped(groups.attention);
  const attention = tile({
    id: 'attention',
    title: 'Needs attention',
    headline:
      groups.attention.length === 0 ? 'All clear' : `${groups.attention.length} needs a look`,
    tone: groups.attention.length === 0 ? 'ok' : 'attention',
    items: failed.shown.map((r, i) => {
      const why = cleanText((r.reason ?? r.error ?? r.summary ?? '').split('\n')[0] ?? '', 120);
      const when = r.finishedAtMs
        ? shortAgo(new Date(r.finishedAtMs).toISOString(), input.nowMs)
        : '';
      return line(
        'attention',
        i,
        {
          label: runRowLabel(r),
          detail: [why, when].filter(Boolean).join(' · '),
          tone: 'attention',
        },
        { command: 'escurel.openRun', args: [{ runId: r.runId }] },
      );
    }),
    more: failed.more,
    empty: 'No run has failed.',
  });

  // 4. Open items per skill.
  const openTile = tile({
    id: 'open',
    title: 'Open items',
    headline:
      input.open.length === 0
        ? 'No records yet'
        : `${input.open.length} ${plural(input.open.length, 'kind', 'kinds')} of work`,
    tone: 'neutral',
    items: input.open.slice(0, TILE_LINES).map((o, i) =>
      line(
        'open',
        i,
        {
          label: o.title,
          detail: `${o.more ? `${o.count}+` : o.count} ${!o.more && o.count === 1 ? 'record' : 'records'}`,
          tone: 'neutral',
        },
        { command: 'escurel.viewSkill', args: [o.skill] },
      ),
    ),
    more: input.open.length > TILE_LINES ? input.open.length - TILE_LINES : undefined,
    empty: 'There are no records yet.',
  });

  // 5. Recently finished.
  const done = capped(input.runs.filter((r) => r.state === 'succeeded'));
  const recent = tile({
    id: 'recent',
    title: 'Recently finished',
    headline:
      insightLine(input.runs, input.nowMs)?.replace('Last 24 h: ', 'Last 24 h · ') ??
      'Nothing finished yet',
    tone: 'neutral',
    items: done.shown.map((r, i) =>
      line(
        'recent',
        i,
        { label: runRowLabel(r), detail: runDescription(r, input.nowMs), tone: 'ok' },
        { command: 'escurel.openRun', args: [{ runId: r.runId }] },
      ),
    ),
    more: done.more,
    empty: 'No agent has finished anything yet.',
  });

  return {
    view: {
      tiles: [decisions, agents, attention, openTile, recent],
      focusOn: input.focusOn,
      updatedAt: new Date(input.nowMs).toISOString(),
    },
    actions,
  };
}
