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

export interface UsageChip {
  text: string;
  /** The accessible name: the chip's abbreviations spelled out. */
  label: string;
  title: string;
}

export function usageChip(usage: RunUsage): UsageChip {
  const cost = usage.costUsd !== undefined ? formatCost(usage.costUsd) : undefined;
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
