import { createHash } from 'node:crypto';
import { chartRefs, chartKey } from '../shared/markdown';
import { evolveOrigin } from './holdoutClient';
import { fetchPage } from './scenarioDiff';
import type { TokenRefresher } from '../auth/refresher';

/**
 * Charts for a comparison page. The page only NAMES a chart (`evolve-chart:<comparison>/<chart>`);
 * the host fetches its SVG from Evolve with the user's own token and shows it only when everything
 * lines up, so a page can neither make the viewer load anything nor show a chart Evolve did not
 * draw for that result. No `vscode` import: tested without one.
 */

/** What Evolve returns for one chart of a comparison. */
export interface FetchedChart {
  resultSha256: string;
  svg: string;
  svgSha256: string;
  alt: string;
}

export interface ResolvedChart {
  src: string;
  alt: string;
}

/** Same ceiling as Evolve applies when it stores a chart. */
export const MAX_SVG_BYTES = 256 * 1024;
/** A comparison page has a couple of charts; more than this is not a comparison page. */
export const MAX_CHARTS = 4;

/**
 * An image shown from `data:` runs no script, but the markup still should carry nothing active, so a
 * future change to how it is shown cannot turn a stored chart into a way in. Mirrors the check Evolve
 * makes before it stores one (`evolve_eval::charts::svg_is_inert`).
 */
export function svgIsInert(svg: string): boolean {
  if (svg.length > MAX_SVG_BYTES || !svg.startsWith('<svg')) return false;
  const lower = svg.toLowerCase();
  const forbidden = [
    '<script',
    '<foreignobject',
    '<image',
    '<style',
    '<use',
    '<iframe',
    '<!doctype',
    '<!entity',
    'javascript:',
    'href=',
    'data:',
  ];
  if (forbidden.some((f) => lower.includes(f))) return false;
  return !/\son[a-z]+=/.test(lower);
}

export function parseFetchedChart(value: unknown): FetchedChart {
  const body = (value ?? {}) as Record<string, unknown>;
  const chart = (body.chart ?? {}) as Record<string, unknown>;
  if (
    typeof body.result_sha256 !== 'string' ||
    typeof chart.svg !== 'string' ||
    typeof chart.svg_sha256 !== 'string' ||
    typeof chart.alt !== 'string'
  )
    throw new Error('Evolve returned a chart this extension does not understand.');
  return {
    resultSha256: body.result_sha256,
    svg: chart.svg,
    svgSha256: chart.svg_sha256,
    alt: chart.alt,
  };
}

/** Read one chart from Evolve (owner only: anyone else gets the same not-found as for no chart). */
export async function fetchChart(
  endpoint: string,
  refresher: TokenRefresher,
  comparison: string,
  chart: string,
): Promise<FetchedChart> {
  return parseFetchedChart(
    await fetchPage(evolveOrigin(endpoint), refresher, { comparison, chart }),
  );
}

/** Why a fetched chart may not be shown, or undefined when it may. */
export function chartProblem(fetched: FetchedChart, pageResultSha256: string): string | undefined {
  // The page is only trusted to describe a result that carries that record's hash.
  if (fetched.resultSha256 !== pageResultSha256)
    return 'the chart belongs to a different result than this page describes';
  if (createHash('sha256').update(fetched.svg).digest('hex') !== fetched.svgSha256)
    return 'the chart does not match its recorded hash';
  if (!svgIsInert(fetched.svg)) return 'the chart markup is not plain';
  return undefined;
}

export interface ChartPageInput {
  skill: string;
  body: string;
  frontmatter: Record<string, unknown> | undefined;
}

/**
 * The charts to show for a page, keyed for the renderer. Only a completed `evolve_comparison` page
 * that carries a result hash qualifies, and only references to ITS OWN comparison are followed.
 * Anything that fails a check is left out (the page then shows the chart's description) and named in
 * `problems` for the log.
 */
export async function resolvePageCharts(
  page: ChartPageInput,
  fetchOne: (comparison: string, chart: string) => Promise<FetchedChart>,
): Promise<{ charts: Record<string, ResolvedChart>; problems: string[] }> {
  const charts: Record<string, ResolvedChart> = {};
  const problems: string[] = [];
  const fm = page.frontmatter ?? {};
  const resultSha = fm.result_sha256;
  if (
    page.skill !== 'evolve_comparison' ||
    fm.status !== 'completed' ||
    typeof fm.id !== 'string' ||
    typeof resultSha !== 'string' ||
    !/^[0-9a-f]{64}$/.test(resultSha)
  )
    return { charts, problems };
  const refs = chartRefs(page.body)
    .filter((r) => r.comparison === fm.id)
    .slice(0, MAX_CHARTS);
  await Promise.all(
    refs.map(async (ref) => {
      const key = chartKey(ref.comparison, ref.chart);
      try {
        const fetched = await fetchOne(ref.comparison, ref.chart);
        const problem = chartProblem(fetched, resultSha);
        if (problem) {
          problems.push(`${key}: ${problem}`);
          return;
        }
        charts[key] = {
          src: `data:image/svg+xml;base64,${Buffer.from(fetched.svg, 'utf8').toString('base64')}`,
          alt: fetched.alt,
        };
      } catch (e) {
        problems.push(`${key}: ${e instanceof Error ? e.message : String(e)}`);
      }
    }),
  );
  return { charts, problems };
}

/**
 * A fetcher that asks Evolve once per chart: a page reloads on every live event, and a stored chart
 * never changes. `clear` drops everything (a different gateway or user must not see the old one's
 * charts); a failed fetch is not remembered, so the next load tries again.
 */
export function cachingChartFetcher(
  fetchOne: (comparison: string, chart: string) => Promise<FetchedChart>,
): { fetch: (comparison: string, chart: string) => Promise<FetchedChart>; clear: () => void } {
  const cache = new Map<string, Promise<FetchedChart>>();
  return {
    fetch: (comparison, chart) => {
      const key = chartKey(comparison, chart);
      let pending = cache.get(key);
      if (!pending) {
        pending = fetchOne(comparison, chart);
        pending.catch(() => cache.delete(key));
        cache.set(key, pending);
      }
      return pending;
    },
    clear: () => cache.clear(),
  };
}
