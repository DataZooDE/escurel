// The Agents view's rows, as pure functions (no `vscode`): agent profiles and what they ran.
import type { Instance, UsageReportResponse } from '../client/types';
import { pageFile } from '../shared/pageId';
import { cleanText } from '../shared/untrustedText';
import { successRate } from '../shared/usageWords';

/** An agent profile (references/01, "Agent profiles"): who a run executes as, and how far it may go. */
export interface AgentProfile {
  id: string;
  pageId: string;
  harness?: string;
  model?: string;
  /** The skills it may run (the ids of its `[[skill::x]]` links). */
  skills: string[];
  /** `scopes.tools`; absent = no tool limit beyond the skill's own. */
  tools?: string[];
  /** `scopes.write` page-id globs; absent = no extra write limit. */
  write?: string[];
  maxConcurrent?: number;
}

const text = (v: unknown, max = 120): string | undefined =>
  typeof v === 'string' && v.trim() ? cleanText(v, max) : undefined;
const list = (v: unknown): string[] =>
  (Array.isArray(v) ? v : v === undefined ? [] : [v]).flatMap((x) => {
    const t = text(x, 200);
    return t ? [t] : [];
  });
/** `[[skill::note]]` → `note`; anything else as written. */
const skillOf = (link: string) => /^\[\[skill::([^\]|#@]+)/.exec(link)?.[1] ?? link;

export function agentProfile(instance: Instance): AgentProfile {
  const fm = instance.frontmatter;
  const scopes =
    fm.scopes && typeof fm.scopes === 'object' ? (fm.scopes as Record<string, unknown>) : {};
  const harness = text(fm.harness);
  const model = text(fm.model);
  const max = typeof fm.max_concurrent === 'number' ? fm.max_concurrent : undefined;
  return {
    id: text(fm.id) ?? cleanText(pageFile(instance.page_id), 120),
    pageId: instance.page_id,
    ...(harness ? { harness } : {}),
    ...(model ? { model } : {}),
    skills: list(fm.skills).map(skillOf),
    ...(scopes.tools !== undefined ? { tools: list(scopes.tools) } : {}),
    ...(scopes.write !== undefined ? { write: list(scopes.write) } : {}),
    ...(max !== undefined ? { maxConcurrent: max } : {}),
  };
}

export interface AgentRow {
  id: string;
  label: string;
  description: string;
  tooltip: string;
  /** The agent page: what a click opens. */
  pageId: string;
}

/** `usage` is the `group_by: [agent]` report, or `admin-required` when the caller may not read it. */
export function agentRows(
  profiles: AgentProfile[],
  opts: { usage: UsageReportResponse | 'admin-required' | undefined; window: string },
): AgentRow[] {
  const byAgent = new Map(
    opts.usage && opts.usage !== 'admin-required'
      ? opts.usage.rows.map((r) => [r.group.agent ?? '', r] as const)
      : [],
  );
  return [...profiles]
    .sort((a, b) => a.id.localeCompare(b.id))
    .map((p) => {
      const engine = [p.harness, p.model].filter(Boolean).join(' · ') || 'runner default agent';
      const used = byAgent.get(`agent:${p.id}`);
      const runs =
        opts.usage === 'admin-required' || opts.usage === undefined
          ? undefined
          : used
            ? [`${used.runs} ${used.runs === 1 ? 'run' : 'runs'}`, successRate(used)]
                .filter(Boolean)
                .join(' · ')
            : 'no runs';
      return {
        id: p.pageId,
        label: p.id,
        pageId: p.pageId,
        description: [engine, runs].filter(Boolean).join(' · '),
        tooltip: [
          p.id,
          `Agent: ${engine}`,
          `Runs: ${p.skills.length ? p.skills.join(', ') : 'no skill (it runs nothing)'}`,
          `Tools: ${p.tools ? p.tools.join(', ') || 'none' : 'no limit beyond the skill'}`,
          `Writes: ${p.write ? p.write.join(', ') || 'nothing' : 'no limit beyond the skill'}`,
          ...(p.maxConcurrent !== undefined ? [`At most ${p.maxConcurrent} at once`] : []),
          opts.usage === 'admin-required'
            ? 'Run counts need an admin sign-in.'
            : runs
              ? `${opts.window}: ${runs}`
              : '',
        ]
          .filter(Boolean)
          .join('\n'),
      };
    });
}
