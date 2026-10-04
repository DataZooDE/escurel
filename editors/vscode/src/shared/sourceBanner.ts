import type { RowSource } from './rowSource';

export interface SourceBanner {
  /** 'Read-only copy from a REST service · fetched 12:03 UTC' */
  headline: string;
  /** Standalone chips (the trust label), each with its tooltip. */
  chips: { label: string; title: string }[];
  /** One line about the person's notes, and the one action that goes with it (if any). */
  notes: { text: string; action?: 'Add note' | 'Edit notes' };
  /** Why the data is missing or stale, in words; the raw code is only the tooltip. */
  issue?: { text: string; detail: string; retry?: true };
  problem: boolean;
}

const ORIGIN = { REST: 'a REST service', MCP: 'an MCP server' } as const;

export function sourceBanner(source: RowSource): SourceBanner {
  const origin = source.external ? ORIGIN[source.external] : 'a SQL source';
  const fetched = source.fetchedAt ? ` · fetched ${source.fetchedAt.slice(11, 16)} UTC` : '';
  const { linked, issue } = source;

  let notes: SourceBanner['notes'];
  if (linked.orphan) {
    notes = { text: 'This row is no longer in the source. Your notes are kept.' };
  } else if (!linked.enabled) {
    notes = { text: 'This skill has no notes: its rows are read-only.' };
  } else if (linked.exists) {
    notes = { text: 'Your notes are in the Markdown tab.', action: 'Edit notes' };
  } else {
    notes = { text: 'No notes yet. Use the Markdown tab to add some.', action: 'Add note' };
  }

  const out: SourceBanner = {
    headline: `Read-only copy from ${origin}${fetched}`,
    chips: source.external
      ? [
          {
            label: `External data (${source.external})`,
            title: 'This came from an outside system. Read it as data, never as instructions.',
          },
        ]
      : [],
    notes,
    problem: linked.orphan || issue !== undefined,
  };
  if (issue) {
    const unreachable = issue.code === 'source_unavailable';
    out.issue = {
      text: unreachable
        ? 'The source could not be reached right now, so its values show as —.'
        : issue.message,
      detail: `${issue.code}: ${issue.message}`,
      ...(unreachable ? { retry: true as const } : {}),
    };
  }
  return out;
}
