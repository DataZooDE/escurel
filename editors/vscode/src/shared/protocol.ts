// The host ↔ webview contract (SPEC §5): typed postMessage both ways.
// Shared by both tsconfigs, so nothing here may import `vscode` or Node.

export interface FieldView {
  name: string;
  label: string;
  /** `string | int | float | bool | date | datetime | enum | link` (or what the skill declared). */
  kind: string;
  /** `text | markdown | date | datetime | money | link | badge` — from `fields[].render`, else derived from `kind`. */
  render: string;
  required: boolean;
  value: unknown;
  /** The value as the form shows it. */
  display: string;
  values?: string[];
  /**
   * The `[[skill::id]]` references this value holds, in order. YAML parses a
   * bare `[[skill::id]]` into the nested list `[["skill::id"]]`, and a field
   * may hold several, so this is always a list. Opened through `resolve`,
   * never a guessed page id.
   */
  links?: { skill: string; id: string; wikilink: string }[];
}

export interface ActionView {
  skill: string;
  /** Until BACKEND_GAPS PR-2: "<Skill title> for <instance title> with an agent". */
  label: string;
}

export interface PageModel {
  pageId: string;
  title: string;
  skill: {
    id: string;
    description: string;
    summary?: string;
    autonomy: 'auto' | 'review' | 'confirm';
    layer: string;
    readOnly: boolean;
    backend: string;
  };
  fields: FieldView[];
  summary?: string;
  body: string;
  lastWrittenBy?: string | null;
  /** False until BACKEND_GAPS PR-1 (live personal drafts) lands. */
  editable: boolean;
  actions: ActionView[];
}

export type HostToWebview =
  { type: 'loading' } | { type: 'page'; model: PageModel } | { type: 'error'; message: string };

export type StartMode = 'background' | 'plan' | 'terminal';

export type WebviewToHost =
  | { type: 'ready' }
  | { type: 'open-page'; pageId: string }
  | { type: 'open-wikilink'; wikilink: string }
  | { type: 'view-skill'; skill: string }
  | { type: 'show-raw' }
  | { type: 'refresh' }
  | { type: 'start-skill'; skill: string; mode: StartMode };

// ── the thread and run webviews (SPEC §3.5, §3.6) ────────────────────
//
// Three types, deliberately separate, because they are built by different hands
// against this file as the only contract:
//
//   ThreadView    what the lineage MEANS: a tree of cards with labels, chips and
//                 what a click opens. Produced from `list_lineage`.
//   ThreadLayout  where it all GOES: columns, positions, wire paths. Produced
//                 from a ThreadView, and the canvas draws exactly this.
//   FocusGraph    how a keyboard MOVES through it. Pure, so every rule is a unit
//                 test rather than a DOM experiment.

/** The colour roles SPEC §2 fixes per node kind; never a hex value. */
export type NodeTone = 'skill' | 'instance' | 'event' | 'run' | 'failed' | 'neutral';

export interface ThreadChip {
  text: string;
  tone: NodeTone;
}

/** What clicking a node opens (SPEC §3.5: "Nodes are clickable"). */
export type NodeTarget =
  | { open: 'thread'; rootEventId: string }
  | { open: 'run'; runId: string }
  | { open: 'review'; draftId?: string; changesetId?: string }
  | { open: 'page'; pageId: string }
  | { open: 'nothing' };

export type ThreadNodeKind = 'event' | 'run' | 'changeset' | 'draft';

export interface ThreadNode {
  id: string;
  kind: ThreadNodeKind;
  /** `null` for the root. Nodes whose parent was pruned by ACL hang off the root. */
  parent: string | null;
  children: string[];
  /** The gateway's `state`, verbatim: `inbox`/`processed`, `running`/`processed`/… */
  state: string | null;
  tone: NodeTone;
  title: string;
  subtitle?: string;
  /** Up to four meta lines, as the mock's cards carry. */
  meta: string[];
  chips: ThreadChip[];
  target: NodeTarget;
  /**
   * Present on a changeset or draft that a human can still decide, which is what
   * puts Promote and Discard on the card itself (SPEC §3.5 "Inline gate buttons").
   */
  gate?: { drafts: number; changesetId?: string; draftId?: string };
  /** Collapsed subtrees render as the mock's "… collapsed. Click to expand." row. */
  collapsible: boolean;
}

export interface ThreadView {
  rootEventId: string;
  /** Flat and keyed by `id`; `parent`/`children` carry the tree. */
  nodes: ThreadNode[];
  /** Column headers, left to right, as the mock labels them. */
  columns: string[];
  /** `true` while more pages of the lineage are still being merged in. */
  loadingMore: boolean;
}

export interface LaidOutNode {
  id: string;
  column: number;
  x: number;
  y: number;
  width: number;
  height: number;
  /** Hidden because an ancestor is collapsed; kept so focus order stays stable. */
  hidden: boolean;
}

export interface Wire {
  from: string;
  to: string;
  /** An SVG path in layout coordinates. */
  path: string;
  /** Dashed in the mock: a promoted draft, a sent notice. */
  style: 'solid' | 'promoted' | 'sent';
}

export interface ThreadLayout {
  nodes: LaidOutNode[];
  wires: Wire[];
  /** The whole graph's extent, for Fit. */
  bounds: { width: number; height: number };
  columnHeaders: { label: string; x: number }[];
}

/**
 * Keyboard movement, one entry per node: `→` follows a wire onward, `←` goes back
 * to the parent, `↑↓` move within a column. A collapsed node's hidden children
 * are absent, so focus never lands somewhere invisible.
 */
export interface FocusStep {
  next?: string;
  back?: string;
  up?: string;
  down?: string;
}

export interface FocusGraph {
  /** Where focus starts, and where `Fit` centres. */
  first: string;
  steps: Record<string, FocusStep>;
}

export type ThreadHostToWebview =
  | { type: 'thread-loading'; rootEventId: string }
  | { type: 'thread'; view: ThreadView; layout: ThreadLayout; focus: FocusGraph }
  | { type: 'thread-error'; message: string; canReconnect: boolean }
  /** The outline selected a node: the canvas highlights it and pans to it. */
  | { type: 'thread-select'; nodeId: string };

export type ThreadWebviewToHost =
  | { type: 'ready' }
  | { type: 'open-node'; nodeId: string }
  | { type: 'select-node'; nodeId: string }
  | { type: 'promote'; changesetId?: string; draftId?: string }
  | { type: 'discard'; changesetId?: string; draftId?: string }
  | { type: 'toggle-collapse'; nodeId: string }
  | { type: 'refresh' };

// ── run detail ───────────────────────────────────────────────────────

export interface RunAttempt {
  n: number;
  startedAt?: string;
  endedAt?: string;
  /** `ok | converged | failed | timeout`, from `run-attempt`. */
  outcome: string;
  error?: string;
}

/** A `report_progress` step. `blocked` is the one that needs a human. */
export interface PlanStep {
  step: string;
  status: 'pending' | 'in_progress' | 'completed' | 'blocked';
}

export interface ToolCallRow {
  seq: number;
  tool: string;
  status: 'ok' | 'error' | 'rejected' | string;
  errorCode?: string | null;
  durationMs: number;
  bytes: { request: number; response: number };
  at: string;
}

export interface RunView {
  runId: string;
  /** `running | processed | failed | dead_letter | cancelled | planned`. */
  status: string;
  tone: NodeTone;
  harness?: string;
  model?: string;
  autonomy?: string;
  targetPageId?: string;
  /** Copyable, per SPEC §3.6. */
  traceId?: string;
  startedAt?: string;
  finishedAt?: string;
  depth?: number;
  attempts: RunAttempt[];
  maxAttempts?: number;
  plan: PlanStep[];
  summary?: string;
  /**
   * The run's own count, from `run-finished`. Not the same thing as `calls.length`:
   * per-call rows are attributed by the run-bound token, so a run can honestly
   * report four calls and expose none.
   */
  toolCallCount?: number;
  calls: ToolCallRow[];
  /** `null` when the last page has been read; pass back as `after`. */
  nextAfter: number | null;
}

export type RunHostToWebview =
  | { type: 'run-loading'; runId: string }
  | { type: 'run'; view: RunView }
  | { type: 'run-error'; message: string; canReconnect: boolean };

export type RunWebviewToHost =
  | { type: 'ready' }
  | { type: 'load-more-calls'; after: number }
  | { type: 'open-page'; pageId: string }
  | { type: 'open-thread'; rootEventId: string }
  | { type: 'copy-trace-id'; traceId: string }
  | { type: 'refresh' };
