import type { QueryInstanceRequest, QueryInstanceResponse } from '../client';
import { buildReport, parseReport, reportParams, type ReportModel } from '../shared/report';

/** What the loader needs of the gateway client: two reads. */
interface ReportClient {
  expand(req: { page_id: string }): Promise<{ frontmatter?: unknown }>;
  queryInstance(req: QueryInstanceRequest): Promise<QueryInstanceResponse>;
}

/**
 * The figures a record's report draws for it: the report skill page says which queries to run and
 * how to show them, the record supplies the values. A report that cannot be shown for this record
 * (a value missing, a query refused) is simply left out: a half-drawn figure is worse than none, and
 * the record page is complete without it.
 */
export async function loadReport(
  client: ReportClient,
  report: string,
  record: Record<string, unknown>,
): Promise<ReportModel | undefined> {
  try {
    const page = await client.expand({ page_id: `markdown/skills/${report}.md` });
    const def = parseReport(page.frontmatter);
    if (!def) return undefined;
    const params = reportParams(def, record);
    if (!params) return undefined;
    const ids = [...new Set(Object.values(def.queries))];
    const answers = await Promise.all(ids.map((ref) => client.queryInstance({ ref, params })));
    const byId = new Map(ids.map((id, i) => [id, answers[i]?.rows ?? []]));
    const results = Object.fromEntries(
      Object.entries(def.queries).map(([name, id]) => [name, byId.get(id) ?? []]),
    );
    return buildReport(def, results);
  } catch {
    return undefined;
  }
}
