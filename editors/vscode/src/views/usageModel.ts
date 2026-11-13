// The Usage view's rows, as pure functions (no `vscode`): `usage_report` folded into words.
import { EscurelError } from '../client/errors';
import type { UsageDimension, UsageReportResponse, UsageTotals } from '../client/types';
import { describeError } from '../errors';
import { statusWord } from '../runs/runWording';
import { pageSlug } from '../shared/pageId';
import { cleanText } from '../shared/untrustedText';
import { compactTokens, formatCost, successRate } from '../shared/usageWords';

export type UsageWindow = '24h' | '7d' | '30d';

export const USAGE_WINDOWS: { id: UsageWindow; label: string; hours: number }[] = [
  { id: '24h', label: 'Last 24 hours', hours: 24 },
  { id: '7d', label: 'Last 7 days', hours: 24 * 7 },
  { id: '30d', label: 'Last 30 days', hours: 24 * 30 },
];

export const USAGE_GROUPS: { id: UsageDimension; label: string }[] = [
  { id: 'skill', label: 'skill' },
  { id: 'model', label: 'model' },
  { id: 'agent', label: 'agent' },
  { id: 'day', label: 'day' },
  { id: 'page', label: 'page' },
];

/** `since` (inclusive) and `until` (exclusive) for a window that ends now. */
export function usageRange(window: UsageWindow, now: Date): { since: string; until: string } {
  const hours = USAGE_WINDOWS.find((w) => w.id === window)?.hours ?? 24 * 7;
  return {
    since: new Date(now.getTime() - hours * 3_600_000).toISOString(),
    until: now.toISOString(),
  };
}

export function usageTitle(window: UsageWindow, groupBy: UsageDimension): string {
  const w = USAGE_WINDOWS.find((x) => x.id === window)?.label ?? window;
  return `${w}, by ${USAGE_GROUPS.find((g) => g.id === groupBy)?.label ?? groupBy}`;
}

export interface UsageRow {
  id: string;
  kind: 'total' | 'group';
  label: string;
  description: string;
  tooltip: string;
}

/** `12 runs · 92% ok · 3.4k tokens · $0.12`: the line a row shows. */
export function usageLine(t: UsageTotals): string {
  return [
    `${t.runs} ${t.runs === 1 ? 'run' : 'runs'}`,
    successRate(t),
    `${compactTokens(t.input_tokens + t.output_tokens)} tokens`,
    formatCost(t.cost_usd),
  ]
    .filter(Boolean)
    .join(' · ');
}

/** Every status that ran and the token split, for the tooltip. */
export function usageDetail(t: UsageTotals): string {
  const statuses = Object.entries(t.by_status)
    .filter(([, n]) => n > 0)
    .map(([status, n]) => `${statusWord(status)} ${n}`)
    .join(' · ');
  const n = (v: number) => Math.round(v).toLocaleString('en-US');
  return [
    statuses,
    `${n(t.input_tokens)} input tokens · ${n(t.output_tokens)} output tokens`,
    `${formatCost(t.cost_usd)} in all`,
  ]
    .filter(Boolean)
    .join('\n');
}

/** What a group is called: an agent by its id, a page by its slug, an empty value plainly. */
export function groupLabel(value: string | null | undefined, dim: UsageDimension): string {
  if (!value) return '(not reported)';
  if (dim === 'agent') return cleanText(value.replace(/^agent:/, ''), 120);
  if (dim === 'page') return pageSlug(value);
  return cleanText(value, 120);
}

/**
 * The total first, then one row per group: most expensive first (then most runs), except days,
 * which read newest first. Nothing at all when no run finished in the window.
 */
export function usageRows(report: UsageReportResponse, groupBy: UsageDimension): UsageRow[] {
  if (!report.total.runs) return [];
  const groups = [...report.rows].sort((a, b) =>
    groupBy === 'day'
      ? String(b.group.day ?? '').localeCompare(String(a.group.day ?? ''))
      : b.cost_usd - a.cost_usd || b.runs - a.runs,
  );
  return [
    {
      id: 'total',
      kind: 'total',
      label: 'All runs',
      description: usageLine(report.total),
      tooltip: usageDetail(report.total),
    },
    ...groups.map((row, i): UsageRow => {
      const label = groupLabel(row.group[groupBy], groupBy);
      return {
        id: `${groupBy}:${i}:${label}`,
        kind: 'group',
        label,
        description: usageLine(row),
        tooltip: `${label}\n${usageDetail(row)}`,
      };
    }),
  ];
}

/** A failed read: a non-admin is told how to see usage, not shown an error. */
export function usageFailure(error: unknown): { adminRequired: boolean; message: string } {
  if (error instanceof EscurelError && error.kind === 'admin_required')
    return { adminRequired: true, message: 'Usage needs an admin sign-in.' };
  return { adminRequired: false, message: `Could not read usage: ${describeError(error)}` };
}
