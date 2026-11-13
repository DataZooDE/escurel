// What runs cost, in words: shared by the run-detail chip and the Usage and Agents views (no `vscode`).
import type { RunUsage } from './protocol';

const thousands = (n: number) => Math.round(n).toLocaleString('en-US');

/** `860`, `12.4k`, `3.5M`: a token count that fits a chip. */
export function compactTokens(n: number): string {
  const short = (v: number, unit: string) => `${v.toFixed(1).replace(/\.0$/, '')}${unit}`;
  if (n < 1000) return String(Math.round(n));
  if (n < 1_000_000) return short(n / 1000, 'k');
  return short(n / 1_000_000, 'M');
}

/** `$0.042`, `$12.35`: two significant digits below a dollar, where a run's cost usually is. */
export function formatCost(usd: number): string {
  if (usd === 0) return '$0';
  return usd >= 1 ? `$${usd.toFixed(2)}` : `$${Number(usd.toPrecision(2))}`;
}

/** What the cost of a run, or a run without a price, reads as. */
export const COST_UNAVAILABLE = 'cost unavailable';

const runsWord = (n: number) => `${n} ${n === 1 ? 'run' : 'runs'}`;

/**
 * A usage row's cost: `$0.12`, `$0.12 · 2 runs unpriced`, or `cost unavailable` when no run was
 * priced. `cost_usd` sums the priced runs only, so `$0` beside unpriced runs would be a lie.
 */
export function costWords(t: { cost_usd: number; priced_runs?: number; unpriced_runs?: number }): {
  short: string;
  detail: string;
} {
  const unpriced = t.unpriced_runs ?? 0;
  if (unpriced > 0 && (t.priced_runs ?? 0) === 0)
    return { short: COST_UNAVAILABLE, detail: `${COST_UNAVAILABLE}: no run reported a cost` };
  const cost = formatCost(t.cost_usd);
  if (unpriced === 0) return { short: cost, detail: `${cost} in all` };
  const n = runsWord(unpriced);
  return {
    short: `${cost} · ${n} unpriced`,
    detail: `${cost} in all; ${n} unpriced (${unpriced === 1 ? 'its' : 'their'} harness reported no cost)`,
  };
}

export interface UsageChip {
  text: string;
  /** The accessible name: the chip's abbreviations spelled out. */
  label: string;
  title: string;
}

export function usageChip(usage: RunUsage): UsageChip {
  const cost = usage.costUsd !== undefined ? formatCost(usage.costUsd) : COST_UNAVAILABLE;
  const model = usage.model ? `model ${usage.model}` : undefined;
  const input = `${thousands(usage.inputTokens)} input tokens`;
  const output = `${thousands(usage.outputTokens)} output tokens`;
  return {
    text: [
      `${compactTokens(usage.inputTokens)} in`,
      `${compactTokens(usage.outputTokens)} out`,
      cost,
    ]
      .filter(Boolean)
      .join(' · '),
    label: `Cost: ${[input, output, cost, model].filter(Boolean).join(', ')}`,
    title: [
      `Input ${thousands(usage.inputTokens)} tokens`,
      `output ${thousands(usage.outputTokens)} tokens`,
      cost,
      model,
    ]
      .filter(Boolean)
      .join(' · '),
  };
}

/** `75% ok` from a usage row (`by_status.processed / runs`); empty when nothing ran. */
export function successRate(row: { runs: number; by_status: Record<string, number> }): string {
  if (!row.runs) return '';
  return `${Math.round(((row.by_status.processed ?? 0) / row.runs) * 100)}% ok`;
}
