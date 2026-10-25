// The Chat view's model: pure, no `vscode`. Chat messages in, rows and words out (SPEC §3.11).
import { createHash } from 'node:crypto';
import type { ChatMessage } from '../client';
import type { ChatAnswerStatus as AnswerStatus, ChatRow } from '../shared/protocol';
import { cleanBlock, cleanText } from '../shared/untrustedText';

/**
 * The gateway's page-bound chat group for `pageId` owned by `subject`:
 * `page-chat:v1:<sha256(subject)>:<page_id>` (crates/escurel-index/src/acl.rs).
 */
export function chatGroupId(subject: string, pageId: string): string {
  return `page-chat:v1:${createHash('sha256').update(subject).digest('hex')}:${pageId}`;
}

export type { ChatRow };

export interface FoldOptions {
  /** `false` when the runner's heartbeat is stale or absent; `undefined` when unknown. */
  runnerAlive?: boolean | undefined;
  /** The latest `report_progress` step of the run working on the question, when known. */
  progress?: string | undefined;
}

const NO_AGENT =
  'No agent is running. Your question is saved and will be answered when one starts.';

const strings = (v: unknown): string[] =>
  Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : [];

function statusOf(meta: Record<string, unknown> | undefined): AnswerStatus {
  const s = meta?.status;
  return s === 'failed' || s === 'cancelled' ? s : 'ok';
}

/** Messages (any order, possibly delivered twice by the socket and a reload) → rows, oldest first. */
export function foldChat(messages: readonly ChatMessage[], opts: FoldOptions): ChatRow[] {
  const byId = new Map<string, ChatMessage>();
  for (const m of messages) byId.set(m.msg_id, m);
  const sorted = [...byId.values()].sort((a, b) =>
    a.ts === b.ts ? a.msg_id.localeCompare(b.msg_id) : a.ts.localeCompare(b.ts),
  );
  const answered = new Set(
    sorted
      .filter((m) => m.role === 'assistant')
      .map((m) => m.metadata?.user_msg_id)
      .filter((id): id is string => typeof id === 'string'),
  );
  const rows: ChatRow[] = [];
  for (const m of sorted) {
    if (m.role === 'user') {
      rows.push({ kind: 'user', key: m.msg_id, text: cleanBlock(m.content), ts: m.ts });
    } else if (m.role === 'assistant') {
      const runId = m.metadata?.run_id;
      rows.push({
        kind: 'assistant',
        key: m.msg_id,
        text: cleanBlock(m.content),
        ts: m.ts,
        status: statusOf(m.metadata),
        runId: typeof runId === 'string' ? runId : undefined,
        drafts: strings(m.metadata?.drafts),
      });
    }
  }
  const last = sorted.filter((m) => m.role === 'user').at(-1);
  if (last && !answered.has(last.msg_id)) {
    if (opts.runnerAlive === false) {
      rows.push({ kind: 'notice', key: `notice:${last.msg_id}`, text: NO_AGENT });
    } else {
      const step = opts.progress ? cleanText(opts.progress, 120) : '';
      rows.push({
        kind: 'pending',
        key: `pending:${last.msg_id}`,
        text: step ? `Working on it: ${step}` : 'Working on it…',
      });
    }
  }
  return rows;
}

/** The `[[target]]` wikilinks in `text` (the label after `|` dropped), each once, in order. */
export function wikilinksIn(text: string): string[] {
  const out: string[] = [];
  for (const m of text.matchAll(/\[\[([^\]|]+)(?:\|[^\]]*)?\]\]/g)) {
    const target = m[1]!.trim();
    if (target && !out.includes(target)) out.push(target);
  }
  return out;
}
