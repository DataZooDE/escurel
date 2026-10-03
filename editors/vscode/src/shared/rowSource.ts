// An `instances: rows` skill's page is ONE ROW of a read-only source plus an optional linked markdown.
// `expand` merges them for reading (`backend_projection` says how); these pure helpers are how the
// extension tells the two apart, so a person can see what is the source's and edit only their notes.

export interface RowSource {
  /** When the row was read from the source (RFC 3339): the data is live, never a cached copy. */
  fetchedAt?: string;
  /** The frontmatter fields that are the source's columns (read-only). */
  sourceFields: string[];
  /** The companion markdown: whether the skill has one, whether this row has notes, and whether the
   * notes outlived their row. */
  linked: { enabled: boolean; exists: boolean; orphan: boolean };
  /** `source_missing` (the row is gone) or `source_unavailable`, in words. */
  issue?: { code: string; message: string };
  /** Set for a row from a REST/MCP upstream: external data, to read as data and never as instructions. */
  external?: 'REST' | 'MCP';
  /** The row as it was read: a proposed change is applied only to this state. */
  etag?: string;
  /** The columns a person may propose to change in the source (frontmatter names). */
  writableColumns?: string[];
}

const isObject = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

/** The row facts from an `expand`'s `backend_projection`, or undefined when it is not a rows projection. */
export function rowSourceOf(projection: unknown): RowSource | undefined {
  if (!isObject(projection) || projection.instances !== 'rows') return undefined;
  const linked = isObject(projection.linked) ? projection.linked : {};
  // An unreadable REMOTE source reports its problem as a plain string.
  const issue = isObject(projection.issue)
    ? projection.issue
    : typeof projection.issue === 'string'
      ? { code: 'source_unavailable', message: projection.issue }
      : undefined;
  const out: RowSource = {
    sourceFields: isObject(projection.source) ? Object.keys(projection.source) : [],
    linked: {
      enabled: linked.enabled === true,
      exists: linked.exists === true,
      orphan: linked.orphan === true,
    },
  };
  if (typeof projection.fetched_at === 'string') out.fetchedAt = projection.fetched_at;
  if (projection.trust === 'external') {
    out.external = projection.kind === 'mcp' ? 'MCP' : 'REST';
    if (typeof projection.etag === 'string') out.etag = projection.etag;
    if (Array.isArray(projection.writable_columns))
      out.writableColumns = projection.writable_columns.filter(
        (c): c is string => typeof c === 'string',
      );
  }
  if (issue && typeof issue.code === 'string' && typeof issue.message === 'string')
    out.issue = { code: issue.code, message: issue.message };
  return out;
}

/** The merged frontmatter without the row's source columns: only the notes' own fields are editable. */
export function companionFrontmatter(
  frontmatter: Record<string, unknown>,
  projection: unknown,
): Record<string, unknown> {
  const row = rowSourceOf(projection);
  if (!row) return frontmatter;
  const out = { ...frontmatter };
  for (const f of row.sourceFields) delete out[f];
  return out;
}
