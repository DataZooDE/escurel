import { createHash } from 'node:crypto';
import { describe, expect, it, vi } from 'vitest';
import {
  cachingChartFetcher,
  chartProblem,
  MAX_CHARTS,
  parseFetchedChart,
  resolvePageCharts,
  svgIsInert,
  type FetchedChart,
} from '../../src/evolve/chartImage';

const RESULT = 'a'.repeat(64);
const SVG = '<svg xmlns="http://www.w3.org/2000/svg"><rect width="3" height="4"/></svg>';
const sha = (text: string): string => createHash('sha256').update(text).digest('hex');

function fetched(over: Partial<FetchedChart> = {}): FetchedChart {
  return { resultSha256: RESULT, svg: SVG, svgSha256: sha(SVG), alt: 'Rows changed: a 3', ...over };
}

const body = (...names: string[]): string =>
  names.map((n) => `![alt ${n}](evolve-chart:cmp-1/${n})`).join('\n\n');

const page = (over: Record<string, unknown> = {}, text = body('rows_changed')) => ({
  skill: 'evolve_comparison',
  body: text,
  frontmatter: { id: 'cmp-1', status: 'completed', result_sha256: RESULT, ...over },
});

describe('svgIsInert', () => {
  it('accepts plain shapes and refuses anything active, external or oversized', () => {
    expect(svgIsInert(SVG)).toBe(true);
    for (const bad of [
      '<svg><script>x()</script></svg>',
      '<svg onload="x()"></svg>',
      '<svg ONCLICK="x()"></svg>',
      '<svg><a href="https://x"></a></svg>',
      '<svg><image href="data:x"/></svg>',
      '<svg><foreignObject/></svg>',
      '<svg><style>*{}</style></svg>',
      '<svg><use x="1"/></svg>',
      '<html></html>',
      ' <svg></svg>',
      `<svg>${'x'.repeat(256 * 1024)}</svg>`,
    ])
      expect(svgIsInert(bad), bad.slice(0, 40)).toBe(false);
  });
});

describe('chartProblem', () => {
  it('passes a chart that matches the page, its own hash, and is plain', () => {
    expect(chartProblem(fetched(), RESULT)).toBeUndefined();
  });

  it('refuses a chart of another result, a chart that is not what was recorded, and active markup', () => {
    expect(chartProblem(fetched({ resultSha256: 'b'.repeat(64) }), RESULT)).toMatch(
      /different result/,
    );
    expect(chartProblem(fetched({ svgSha256: sha('other') }), RESULT)).toMatch(/recorded hash/);
    const bad = '<svg><script>x()</script></svg>';
    expect(chartProblem(fetched({ svg: bad, svgSha256: sha(bad) }), RESULT)).toMatch(/not plain/);
  });
});

describe('resolvePageCharts', () => {
  it('returns a data image keyed by comparison and chart, with Evolve’s description', async () => {
    const fetchOne = vi.fn(async () => fetched());
    const { charts, problems } = await resolvePageCharts(page(), fetchOne);
    expect(problems).toEqual([]);
    const image = charts['cmp-1/rows_changed']!;
    expect(image.src).toBe(`data:image/svg+xml;base64,${Buffer.from(SVG).toString('base64')}`);
    expect(image.alt).toBe('Rows changed: a 3');
    expect(fetchOne).toHaveBeenCalledWith('cmp-1', 'rows_changed');
  });

  it('asks Evolve for nothing unless the page is a completed comparison carrying a result hash', async () => {
    const fetchOne = vi.fn(async () => fetched());
    for (const p of [
      { ...page(), skill: 'evolve_experiment' },
      page({ status: 'requested' }),
      page({ status: 'blocked' }),
      page({ result_sha256: undefined }),
      page({ result_sha256: 'not-a-hash' }),
      page({ id: undefined }),
      { ...page(), frontmatter: undefined },
    ]) {
      expect((await resolvePageCharts(p, fetchOne)).charts).toEqual({});
    }
    expect(fetchOne).not.toHaveBeenCalled();
  });

  it('follows only references to its own comparison, so a page cannot name someone else’s chart', async () => {
    const fetchOne = vi.fn(async () => fetched());
    const text =
      '![x](evolve-chart:cmp-OTHER/rows_changed)\n\n![y](evolve-chart:cmp-1/rows_changed)';
    const { charts } = await resolvePageCharts(page({}, text), fetchOne);
    expect(Object.keys(charts)).toEqual(['cmp-1/rows_changed']);
    expect(fetchOne).toHaveBeenCalledTimes(1);
  });

  it('caps how many charts it will fetch', async () => {
    const fetchOne = vi.fn(async () => fetched());
    const names = Array.from({ length: MAX_CHARTS + 3 }, (_, i) => `c${i}`);
    await resolvePageCharts(page({}, body(...names)), fetchOne);
    expect(fetchOne).toHaveBeenCalledTimes(MAX_CHARTS);
  });

  it('leaves out a chart that fails a check or cannot be read, and says why', async () => {
    const text = body('good', 'wrong_result', 'bad_hash', 'gone');
    const fetchOne = vi.fn(async (_c: string, name: string) => {
      if (name === 'wrong_result') return fetched({ resultSha256: 'b'.repeat(64) });
      if (name === 'bad_hash') return fetched({ svgSha256: sha('x') });
      if (name === 'gone') throw new Error('not found');
      return fetched();
    });
    const { charts, problems } = await resolvePageCharts(page({}, text), fetchOne);
    expect(Object.keys(charts)).toEqual(['cmp-1/good']);
    expect(problems.sort()).toEqual([
      'cmp-1/bad_hash: the chart does not match its recorded hash',
      'cmp-1/gone: not found',
      'cmp-1/wrong_result: the chart belongs to a different result than this page describes',
    ]);
  });
});

describe('parseFetchedChart', () => {
  it('reads Evolve’s answer and rejects anything else', () => {
    const ok = parseFetchedChart({
      comparison: 'cmp-1',
      result_sha256: RESULT,
      chart: { name: 'rows_changed', alt: 'a', svg: SVG, svg_sha256: sha(SVG) },
    });
    expect(ok.svgSha256).toBe(sha(SVG));
    for (const bad of [
      null,
      {},
      { result_sha256: RESULT },
      { result_sha256: RESULT, chart: { svg: 1 } },
    ])
      expect(() => parseFetchedChart(bad)).toThrow(/does not understand/);
  });
});

describe('cachingChartFetcher', () => {
  it('asks Evolve once per chart, and again after clear', async () => {
    const inner = vi.fn(async () => fetched());
    const cache = cachingChartFetcher(inner);
    await Promise.all([cache.fetch('cmp-1', 'a'), cache.fetch('cmp-1', 'a')]);
    await cache.fetch('cmp-1', 'a');
    await cache.fetch('cmp-1', 'b');
    expect(inner).toHaveBeenCalledTimes(2);
    cache.clear();
    await cache.fetch('cmp-1', 'a');
    expect(inner).toHaveBeenCalledTimes(3);
  });

  it('does not remember a failure', async () => {
    const inner = vi
      .fn<(c: string, n: string) => Promise<FetchedChart>>()
      .mockRejectedValueOnce(new Error('down'))
      .mockResolvedValue(fetched());
    const cache = cachingChartFetcher(inner);
    await expect(cache.fetch('cmp-1', 'a')).rejects.toThrow('down');
    await expect(cache.fetch('cmp-1', 'a')).resolves.toBeDefined();
    expect(inner).toHaveBeenCalledTimes(2);
  });
});
