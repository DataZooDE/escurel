// The runs control center's model: pure, no `vscode`. Run lifecycle events in, rows and words out.
import type { Event } from '../client';
import { pageSlug } from '../shared/pageId';
import { cleanBlock, cleanOpt } from '../shared/untrustedText';
import { parseGatewayTime } from '../shared/time';
import {
  deriveHealth,
  ESCUREL_RUNNER_STATUS_INTERVAL_MS,
  type RunnerStatusBody,
} from './runnerModel';

export type RunState =
  | 'running'
  | 'planned'
  | 'succeeded'
  | 'failed'
  | 'dead_letter'
  | 'cancelled'
  /** Started, never finished, and the runner no longer lists it as live. */
  | 'unknown';

export interface RunRecord {
  runId: string;
  state: RunState;
  skill?: string | undefined;
  targetPageId?: string | null | undefined;
  rootEventId?: string | null | undefined;
  triggerEventId?: string | undefined;
  harness?: string | undefined;
  startedAtMs?: number | undefined;
  finishedAtMs?: number | undefined;
  durationMs?: number | undefined;
  reason?: string | undefined;
  error?: string | undefined;
  summary?: string | undefined;
  attempts?: number | undefined;
  toolCalls?: number | undefined;
}

export interface FoldOptions {
  nowMs: number;
  /** The run ids the runner's newest status lists as live; absent when there is no status. */
  liveRunIds?: ReadonlySet<string> | undefined;
  /** Trigger event id -> the skill that event was filed under (the skill the run executes). */
  skillByEvent?: ReadonlyMap<string, string> | undefined;
}

interface Acc {
  runId: string;
  started?: Event;
  finished?: Event;
  any: Event;
  attemptStart?: number;
  attemptEnd?: number;
}

const DAY_MS = 86_400_000;

const ms = (raw: unknown): number | undefined => parseGatewayTime(raw)?.getTime();

function json(e: Event | undefined): Record<string, unknown> {
  if (!e?.body) return {};
  try {
    const v: unknown = JSON.parse(e.body);
    return v && typeof v === 'object' ? (v as Record<string, unknown>) : {};
  } catch {
    return {};
  }
}

/** Free text that may span lines (the reason row shows the first, the tooltip all): cleaned and capped. */
const blockOpt = (v: unknown, max: number): string | undefined =>
  typeof v === 'string' && v ? cleanBlock(v, max) || undefined : undefined;

const str = (v: unknown): string | undefined => (typeof v === 'string' && v ? v : undefined);

/** One record per run, newest first, from the `escurel:run` lifecycle events. */
export function foldRuns(events: readonly Event[], opts: FoldOptions): RunRecord[] {
  const byRun = new Map<string, Acc>();
  for (const e of events) {
    if (e.label_skill !== 'escurel:run' || !e.run_id) continue;
    if (e.title !== 'run-started' && e.title !== 'run-attempt' && e.title !== 'run-finished')
      continue;
    const acc = byRun.get(e.run_id) ?? { runId: e.run_id, any: e };
    if (e.title === 'run-started') acc.started = e;
    if (e.title === 'run-finished') acc.finished = e;
    if (e.title === 'run-attempt') {
      const b = json(e);
      const s = ms(b.started_at);
      const t = ms(b.ended_at);
      if (s !== undefined) acc.attemptStart = Math.min(acc.attemptStart ?? s, s);
      if (t !== undefined) acc.attemptEnd = Math.max(acc.attemptEnd ?? t, t);
    }
    byRun.set(e.run_id, acc);
  }

  const out: RunRecord[] = [];
  for (const acc of byRun.values()) {
    const src = acc.started ?? acc.finished ?? acc.any;
    const runner = ((src.provenance ?? {}) as { runner?: Record<string, unknown> }).runner ?? {};
    const body = json(acc.finished);
    const startedAtMs = ms(acc.started?.at);
    const finishedAtMs = ms(acc.finished?.at);

    let state: RunState;
    if (acc.finished) {
      const s = str(body.status);
      state =
        s === 'processed'
          ? 'succeeded'
          : s === 'planned' || s === 'failed' || s === 'dead_letter' || s === 'cancelled'
            ? s
            : 'failed';
    } else if (opts.liveRunIds) {
      state = opts.liveRunIds.has(acc.runId) ? 'running' : 'unknown';
    } else {
      state = 'running';
    }

    let durationMs: number | undefined;
    if (acc.attemptStart !== undefined && acc.attemptEnd !== undefined) {
      durationMs = Math.max(0, acc.attemptEnd - acc.attemptStart);
    } else if (startedAtMs !== undefined && finishedAtMs !== undefined) {
      durationMs = Math.max(0, finishedAtMs - startedAtMs);
    }

    const trigger = str(runner.event_id);
    const pageId = src.instance_page_id || str(runner.target_page_id) || null;
    out.push({
      runId: acc.runId,
      state,
      skill: trigger ? cleanOpt(opts.skillByEvent?.get(trigger), 80) : undefined,
      targetPageId: cleanOpt(pageId, 300) ?? null,
      rootEventId: src.root_event_id ?? str(runner.root_event_id) ?? null,
      triggerEventId: trigger,
      harness: cleanOpt(runner.harness, 60),
      startedAtMs,
      finishedAtMs,
      durationMs,
      reason: blockOpt(body.reason, 1000),
      error: blockOpt(body.error, 1000),
      summary: blockOpt(body.summary, 1000),
      attempts: typeof body.attempts === 'number' ? body.attempts : undefined,
      toolCalls: typeof body.tool_calls === 'number' ? body.tool_calls : undefined,
    });
  }
  const when = (r: RunRecord) => r.finishedAtMs ?? r.startedAtMs ?? 0;
  return out.sort((a, b) => when(b) - when(a));
}

export interface RunGroups {
  running: RunRecord[];
  waiting: RunRecord[];
  attention: RunRecord[];
  history: RunRecord[];
}

const ENDED: ReadonlySet<RunState> = new Set([
  'succeeded',
  'failed',
  'dead_letter',
  'cancelled',
  'unknown',
]);

/**
 * Sections. A failure or a plan is "handled" once a NEWER run exists for the same trigger event: a
 * retry, or the approved run of a plan. Without that a retried dead letter would sit under "Needs
 * attention" for ever.
 */
export function groupRuns(records: readonly RunRecord[], _nowMs: number): RunGroups {
  const newestByTrigger = new Map<string, RunRecord>();
  for (const r of records) {
    if (!r.triggerEventId) continue;
    const cur = newestByTrigger.get(r.triggerEventId);
    const t = r.finishedAtMs ?? r.startedAtMs ?? 0;
    if (!cur || t > (cur.finishedAtMs ?? cur.startedAtMs ?? 0))
      newestByTrigger.set(r.triggerEventId, r);
  }
  const superseded = (r: RunRecord) => {
    const newest = r.triggerEventId ? newestByTrigger.get(r.triggerEventId) : undefined;
    return !!newest && newest.runId !== r.runId;
  };
  return {
    running: records.filter((r) => r.state === 'running'),
    waiting: records.filter((r) => r.state === 'planned' && !superseded(r)),
    attention: records.filter(
      (r) => (r.state === 'failed' || r.state === 'dead_letter') && !superseded(r),
    ),
    history: records.filter((r) => ENDED.has(r.state)),
  };
}

/** The day windows the filter offers: "what did the agent do yesterday". */
export type RunsRange = 'today' | 'yesterday' | '7d';

export interface RunsFilter {
  states?: readonly RunState[] | undefined;
  skill?: string | undefined;
  text?: string | undefined;
  /** Only the runs that worked on this page: "Runs for this record". */
  pageId?: string | undefined;
  range?: RunsRange | undefined;
}

/** [from, to) in epoch ms for a range: calendar days in UTC, so the same words mean the same window everywhere. */
export function rangeBounds(range: RunsRange, nowMs: number): [number, number] {
  const startOfToday = nowMs - (nowMs % DAY_MS);
  switch (range) {
    case 'today':
      return [startOfToday, startOfToday + DAY_MS];
    case 'yesterday':
      return [startOfToday - DAY_MS, startOfToday];
    case '7d':
      return [nowMs - 7 * DAY_MS, nowMs + 1];
  }
}

const RANGE_WORD: Record<RunsRange, string> = {
  today: 'today',
  yesterday: 'yesterday',
  '7d': 'last 7 days',
};

export function applyFilter(
  records: readonly RunRecord[],
  f: RunsFilter,
  nowMs: number = Date.now(),
): RunRecord[] {
  const text = f.text?.trim().toLowerCase();
  const bounds = f.range ? rangeBounds(f.range, nowMs) : undefined;
  return records.filter((r) => {
    if (f.states?.length && !f.states.includes(r.state)) return false;
    if (f.skill && r.skill !== f.skill) return false;
    if (f.pageId && r.targetPageId !== f.pageId) return false;
    if (bounds) {
      const at = r.finishedAtMs ?? r.startedAtMs;
      if (at === undefined || at < bounds[0] || at >= bounds[1]) return false;
    }
    if (text) {
      const hay = [
        r.skill,
        r.targetPageId ? pageSlug(r.targetPageId) : '',
        r.reason,
        r.error,
        r.summary,
      ]
        .filter(Boolean)
        .join(' ')
        .toLowerCase();
      if (!hay.includes(text)) return false;
    }
    return true;
  });
}

/** "Last 24 h: 12 runs · 11 ok · 1 failed · avg 6 s", or nothing when nothing finished. */
export function insightLine(records: readonly RunRecord[], nowMs: number): string | undefined {
  const recent = records.filter(
    (r) =>
      r.finishedAtMs !== undefined && nowMs - r.finishedAtMs <= DAY_MS && r.state !== 'planned',
  );
  if (recent.length === 0) return undefined;
  const ok = recent.filter((r) => r.state === 'succeeded').length;
  const failed = recent.filter((r) => r.state === 'failed' || r.state === 'dead_letter').length;
  const cancelled = recent.filter((r) => r.state === 'cancelled').length;
  const timed = recent.filter((r) => r.durationMs !== undefined);
  const parts = [`${recent.length} run${recent.length === 1 ? '' : 's'}`];
  if (ok) parts.push(`${ok} ok`);
  if (failed) parts.push(`${failed} failed`);
  if (cancelled) parts.push(`${cancelled} cancelled`);
  if (timed.length > 0) {
    const avg = timed.reduce((s, r) => s + (r.durationMs ?? 0), 0) / timed.length;
    parts.push(`avg ${formatMs(avg)}`);
  }
  return `Last 24 h: ${parts.join(' · ')}`;
}

/** The same sentence as two short lines, so a narrow panel never cuts it mid-word. */
export function insightLines(
  records: readonly RunRecord[],
  nowMs: number,
): [string, string | undefined] | undefined {
  const line = insightLine(records, nowMs);
  if (!line) return undefined;
  const [head, ...rest] = line.replace('Last 24 h: ', '').split(' · ');
  return [`Last 24 h: ${head}`, rest.length ? rest.join(' · ') : undefined];
}

/** A length of time at the scale a person reads it. */
export function formatMs(msValue: number): string {
  if (msValue < 1000) return `${Math.round(msValue)} ms`;
  const s = Math.round(msValue / 1000);
  if (s < 60) return `${s} s`;
  const rest = s % 60;
  return rest ? `${Math.floor(s / 60)} min ${rest} s` : `${Math.floor(s / 60)} min`;
}

/** "supplier-risk · order-4500123": what ran on what. Never an id. */
export function runLabel(r: Pick<RunRecord, 'skill' | 'targetPageId'>): string {
  const page = r.targetPageId ? pageSlug(r.targetPageId) : '';
  if (r.skill && page) return `${r.skill} · ${page}`;
  return r.skill || page || 'Run';
}

/** The outcome, first in a row: a narrow panel cuts the end, and the end is the skill and page. */
const ROW_WORD: Record<RunState, string> = {
  running: 'Running',
  planned: 'Plan ready',
  succeeded: 'Done',
  failed: 'Failed',
  dead_letter: 'Gave up',
  cancelled: 'Cancelled',
  unknown: 'No result',
};

/** "Failed · supplier-risk · order-4500123": what happened first, then to what. */
export function runRowLabel(r: Pick<RunRecord, 'state' | 'skill' | 'targetPageId'>): string {
  return `${ROW_WORD[r.state]} · ${runLabel(r)}`;
}

const STATE_WORD: Record<RunState, string> = {
  running: 'running',
  planned: 'plan ready',
  succeeded: 'succeeded',
  failed: 'failed',
  dead_letter: 'failed for good',
  cancelled: 'cancelled',
  unknown: 'no result recorded',
};

export function stateWord(state: RunState): string {
  return STATE_WORD[state];
}

/** The row's second line: what happened, how long it took, how long ago. */
/** "now", "3 m", "5 h", "2 d": a relative time short enough for a narrow row. Empty when unreadable. */
export function shortAgo(raw: unknown, nowMs: number): string {
  const then = parseGatewayTime(raw)?.getTime();
  if (then === undefined) return '';
  const minutes = Math.max(0, Math.floor((nowMs - then) / 60_000));
  if (minutes < 1) return 'now';
  if (minutes < 60) return `${minutes} m`;
  const hours = Math.floor(minutes / 60);
  return hours < 24 ? `${hours} h` : `${Math.floor(hours / 24)} d`;
}

/** The row's description: only the time, because the outcome is already the first word of the label. */
export function runDescription(r: RunRecord, nowMs: number): string {
  const ago =
    r.finishedAtMs !== undefined ? shortAgo(new Date(r.finishedAtMs).toISOString(), nowMs) : '';
  // Elapsed time only: the spinner and the section already say it is running.
  if (r.state === 'running')
    return r.startedAtMs !== undefined ? formatMs(Math.max(0, nowMs - r.startedAtMs)) : '';
  if (r.state === 'succeeded')
    return [r.durationMs !== undefined ? formatMs(r.durationMs) : '', ago]
      .filter(Boolean)
      .join(' · ');
  return ago;
}

export interface RunnerDescription {
  /** One sentence for the top of the view. */
  text: string;
  paused: boolean;
  /** Which engine runs the agents, for a tooltip; never in the sentence. */
  engine?: string | undefined;
  /** What the dispatch row offers: the action, or why the person cannot take it. */
  dispatchHint: string;
  health: 'ok' | 'stale' | 'draining' | 'none';
}

/** Runner health and dispatch state, in words. */
export function describeRunner(
  status: { at?: string | null; body?: string | RunnerStatusBody | null } | null | undefined,
  nowMs: number,
  opts: { isAdmin: boolean; tenant?: string | undefined; intervalMs?: number | undefined },
): RunnerDescription {
  if (!status) {
    return {
      text: 'No agents have reported yet.',
      paused: false,
      dispatchHint: '',
      health: 'none',
    };
  }
  const body: RunnerStatusBody =
    typeof status.body === 'string' ? safeBody(status.body) : (status.body ?? {});
  const health = deriveHealth(status, nowMs, {
    intervalMs: opts.intervalMs ?? ESCUREL_RUNNER_STATUS_INTERVAL_MS,
  });
  const seconds = status.at
    ? Math.max(0, Math.round((nowMs - (parseGatewayTime(status.at)?.getTime() ?? nowMs)) / 1000))
    : 0;
  const paused = !!opts.tenant && (body.paused_tenants ?? []).includes(opts.tenant);
  const lead =
    health.state === 'stale'
      ? 'Agents are not responding'
      : health.state === 'draining'
        ? 'Agents are shutting down'
        : 'Agents are running';
  const parts = [
    lead,
    seconds < 2 ? 'last seen just now' : `last seen ${formatMs(seconds * 1000)} ago`,
  ];
  if (paused) parts[0] = 'Agents are paused: new work waits';
  const verb = paused ? 'Resume' : 'Pause';
  // Honest about the demo: the echo engine folds events by rule, there is no model behind it. Only
  // in the tooltip: the name of the engine is a technical detail.
  const engine = body.harness
    ? body.harness === 'echo'
      ? 'Agent engine: echo (demo, no AI model)'
      : `Agent engine: ${body.harness}`
    : undefined;
  return {
    text: parts.join(' · '),
    paused,
    engine,
    dispatchHint: opts.isAdmin
      ? `${verb} agents`
      : `Only an admin can ${verb.toLowerCase()} agents.`,
    health: health.state,
  };
}

function safeBody(raw: string): RunnerStatusBody {
  try {
    return JSON.parse(raw) as RunnerStatusBody;
  } catch {
    return {};
  }
}

// --- the tree -------------------------------------------------------------------------------

export type RunsNodeKind =
  'insight' | 'dispatch' | 'group' | 'run' | 'reason' | 'more' | 'empty' | 'error';

export interface RunsNode {
  /** Stable, so VS Code keeps a group expanded across refreshes. */
  id: string;
  kind: RunsNodeKind;
  label: string;
  description?: string | undefined;
  tooltip?: string | undefined;
  contextValue?: string | undefined;
  state?: RunState | undefined;
  /** The fields the commands read from a tree row (`controlRequest`, `openRun`, `approvePlan`, `openThread`). */
  runId?: string | undefined;
  /** The run's TRIGGER event: what `requeue` names. */
  eventId?: string | undefined;
  rootEventId?: string | undefined;
  pageId?: string | undefined;
  skill?: string | undefined;
  children?: RunsNode[] | undefined;
  expanded?: boolean | undefined;
}

/** How many failures the section lists; the rest are one click away in History, filtered. */
const ATTENTION_LIMIT = 10;

export interface TreeInput {
  records: readonly RunRecord[];
  filter: RunsFilter;
  nowMs: number;
  /** How many history rows to show. */
  historyLimit: number;
  /** Whether the gateway holds older run events than the ones loaded. */
  hasMoreHistory: boolean;
  runner: RunnerDescription | undefined;
  isAdmin: boolean;
  error?: string | undefined;
}

/** Everything a person needs to know about one run, with the id only here. */
export function tooltipFor(r: RunRecord, nowMs: number): string {
  const lines = [`${runLabel(r)} — ${stateWord(r.state)}`];
  if (r.startedAtMs !== undefined)
    lines.push(
      `Started: ${new Date(r.startedAtMs).toISOString().replace('T', ' ').slice(0, 19)} UTC`,
    );
  if (r.durationMs !== undefined) lines.push(`Took: ${formatMs(r.durationMs)}`);
  else if (r.state === 'running' && r.startedAtMs !== undefined)
    lines.push(`Running for: ${formatMs(Math.max(0, nowMs - r.startedAtMs))}`);
  if (r.harness) lines.push(`Agent engine: ${r.harness}`);
  if (r.attempts !== undefined) lines.push(`Attempts: ${r.attempts}`);
  if (r.toolCalls !== undefined) lines.push(`Tool calls: ${r.toolCalls}`);
  if (r.reason) lines.push(`Reason: ${r.reason}`);
  if (r.error) lines.push(`Error: ${r.error}`);
  if (r.summary) lines.push(`Result: ${r.summary}`);
  lines.push(`Run id: ${r.runId}`);
  return lines.join('\n');
}

function runNode(r: RunRecord, nowMs: number, section: string): RunsNode {
  const reason = [
    ...new Set([r.reason, r.error].map((x) => (x ?? '').trim()).filter(Boolean)),
  ].join(' — ');
  const reasonRow: RunsNode | undefined =
    section === 'attention' && reason
      ? {
          id: `reason:${r.runId}`,
          kind: 'reason',
          label: reason.split('\n')[0]!.slice(0, 160),
          tooltip: reason,
        }
      : undefined;
  const context =
    r.state === 'running'
      ? 'run.running'
      : r.state === 'planned'
        ? 'run.planned'
        : r.state === 'failed' || r.state === 'dead_letter'
          ? 'run.failed'
          : 'run.done';
  return {
    id: `run:${section}:${r.runId}`,
    kind: 'run',
    label: runRowLabel(r),
    description: runDescription(r, nowMs),
    tooltip: tooltipFor(r, nowMs),
    contextValue: context,
    state: r.state,
    runId: r.runId,
    eventId: r.triggerEventId,
    rootEventId: r.rootEventId ?? undefined,
    pageId: r.targetPageId ?? undefined,
    skill: r.skill,
    ...(reasonRow ? { children: [reasonRow], expanded: true } : {}),
  };
}

const empty = (id: string, label: string): RunsNode => ({ id, kind: 'empty', label });

/** The control center's rows: the dispatch row, then the four sections. */
export function buildRunsTree(input: TreeInput): RunsNode[] {
  if (input.error) {
    return [
      {
        id: 'error',
        kind: 'error',
        label: `Couldn't load runs: ${input.error}. Try again.`,
        contextValue: 'runs.error',
      },
    ];
  }
  // "Runs for this record" narrows EVERY section to that page (the other filters only narrow History: what
  // is running or needs you is never hidden by a state or a day).
  const records = input.filter.pageId
    ? input.records.filter((r) => r.targetPageId === input.filter.pageId)
    : input.records;
  const groups = groupRuns(records, input.nowMs);
  const out: RunsNode[] = [];

  if (input.runner && input.runner.health !== 'none') {
    out.push({
      id: 'dispatch',
      kind: 'dispatch',
      label: input.runner.paused ? 'Agents are paused' : 'Agents are running',
      // The button is there for an admin; for anyone else the short reason, and the sentence on hover.
      description: input.isAdmin ? undefined : input.runner.paused ? 'admins only' : 'admins only',
      tooltip: [input.runner.text, input.runner.engine, input.runner.dispatchHint]
        .filter(Boolean)
        .join('\n'),
      contextValue: input.runner.paused ? 'dispatch.paused' : 'dispatch.running',
    });
  }

  const insight = insightLines(records, input.nowMs);
  if (insight) {
    const full = insightLine(records, input.nowMs);
    out.push({ id: 'insight', kind: 'insight', label: insight[0], tooltip: full });
    if (insight[1])
      out.push({ id: 'insight:detail', kind: 'insight', label: insight[1], tooltip: full });
  }

  out.push({
    id: 'group:running',
    kind: 'group',
    label: 'Running now',
    description: String(groups.running.length),
    expanded: true,
    children: groups.running.length
      ? groups.running.map((r) => runNode(r, input.nowMs, 'running'))
      : [empty('empty:running', 'Nothing is running.')],
  });

  if (groups.waiting.length) {
    out.push({
      id: 'group:waiting',
      kind: 'group',
      label: 'Waiting for you',
      description: String(groups.waiting.length),
      expanded: true,
      children: groups.waiting.map((r) => runNode(r, input.nowMs, 'waiting')),
    });
  }
  if (groups.attention.length) {
    const shown = groups.attention.slice(0, ATTENTION_LIMIT);
    const rest = groups.attention.length - shown.length;
    out.push({
      id: 'group:attention',
      kind: 'group',
      label: 'Needs attention',
      description: String(groups.attention.length),
      expanded: true,
      children: [
        ...shown.map((r) => runNode(r, input.nowMs, 'attention')),
        ...(rest > 0
          ? [
              {
                id: 'more:attention',
                kind: 'more' as const,
                label: `Show ${rest} older failure${rest === 1 ? '' : 's'} in History`,
                contextValue: 'runs.showFailed',
              },
            ]
          : []),
      ],
    });
  }

  const history = applyFilter(groups.history, input.filter, input.nowMs);
  const shown = history.slice(0, input.historyLimit);
  const children: RunsNode[] = shown.map((r) => runNode(r, input.nowMs, 'history'));
  const hidden = history.length - shown.length;
  if (children.length === 0) {
    const filtered = !!(
      input.filter.states?.length ||
      input.filter.skill ||
      input.filter.text ||
      input.filter.pageId ||
      input.filter.range
    );
    children.push(
      empty('empty:history', filtered ? 'No runs match the filter.' : 'No finished runs yet.'),
    );
  } else if (hidden > 0) {
    children.push({
      id: 'more',
      kind: 'more',
      label: `Load ${Math.min(hidden, 25)} more…`,
      contextValue: 'runs.more',
    });
  } else if (input.hasMoreHistory) {
    children.push({ id: 'more', kind: 'more', label: 'Load more…', contextValue: 'runs.more' });
  }
  out.push({
    id: 'group:history',
    kind: 'group',
    label: 'History',
    description: String(history.length),
    expanded: true,
    children,
  });
  return out;
}

// --- the filter's pick list -----------------------------------------------------------------

export interface FilterPick {
  id: string;
  label: string;
  description?: string;
  picked: boolean;
}

/** The pick list for "Filter runs…": states, the skills seen in the loaded runs, and a text search. */
export function filterPickItems(skills: readonly string[], current: RunsFilter): FilterPick[] {
  const has = (s: RunState) => !!current.states?.includes(s);
  return [
    { id: 'state:succeeded', label: 'Succeeded', picked: has('succeeded') },
    {
      id: 'state:failed',
      label: 'Failed',
      description: 'including runs that failed for good',
      picked: has('failed'),
    },
    { id: 'state:cancelled', label: 'Cancelled', picked: has('cancelled') },
    { id: 'range:today', label: 'Today', picked: current.range === 'today' },
    { id: 'range:yesterday', label: 'Yesterday', picked: current.range === 'yesterday' },
    { id: 'range:7d', label: 'Last 7 days', picked: current.range === '7d' },
    ...skills.map((s) => ({
      id: `skill:${s}`,
      label: s,
      description: 'skill',
      picked: current.skill === s,
    })),
    {
      id: 'text',
      label: 'Search by text…',
      description: 'a page, a reason, a result',
      picked: !!current.text,
    },
  ];
}

/** The filter the picks stand for. "Failed" also means failed for good: a person does not tell them apart. */
export function filterFromPicks(
  picks: readonly string[],
  text: string | undefined,
  keep: Pick<RunsFilter, 'pageId'> = {},
): RunsFilter {
  const states: RunState[] = [];
  for (const id of picks) {
    if (id === 'state:succeeded') states.push('succeeded');
    if (id === 'state:failed') states.push('failed', 'dead_letter');
    if (id === 'state:cancelled') states.push('cancelled');
  }
  const skill = picks.find((id) => id.startsWith('skill:'))?.slice('skill:'.length);
  const t = picks.includes('text') ? text?.trim() : undefined;
  const range = (['today', 'yesterday', '7d'] as const).find((r) => picks.includes(`range:${r}`));
  return {
    ...(states.length ? { states } : {}),
    ...(range ? { range } : {}),
    ...(skill ? { skill } : {}),
    ...(t ? { text: t } : {}),
    ...(keep.pageId ? { pageId: keep.pageId } : {}),
  };
}

/** The few words the view shows while a filter is on: "failed · supplier-risk". */
export function filterNote(f: RunsFilter): string {
  // The pick "Failed" stands for failed AND failed for good: say it once.
  const states = (f.states ?? []).filter(
    (s) => !(s === 'dead_letter' && f.states?.includes('failed')),
  );
  return [
    f.pageId ? pageSlug(f.pageId) : undefined,
    ...states.map((s) => stateWord(s)),
    f.range ? RANGE_WORD[f.range] : undefined,
    f.skill,
    f.text ? `“${f.text}”` : undefined,
  ]
    .filter(Boolean)
    .join(' · ');
}
