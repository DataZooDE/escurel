// Where a skill's data comes from, in the words a person uses. The raw kind (`sql_view`, `openapi`,
// `mcp`) is how the gateway names it; it is never shown.

/** `sql_view` -> "SQL table", `openapi` -> "REST API", `mcp` -> "MCP tool". */
export function backendLabel(kind: string): string {
  switch (kind) {
    case 'markdown':
      return 'Pages in this knowledge base';
    case 'sql_view':
      return 'SQL table';
    case 'openapi':
      return 'REST API';
    case 'mcp':
      return 'MCP tool';
    case 'document':
      return 'Uploaded documents';
    default:
      return kind;
  }
}

/** The short form for a chip beside a skill name. */
export function backendChip(kind: string): string {
  return kind === 'document' ? 'documents' : backendLabel(kind);
}

/** An ISO-8601 duration in words: `P90D` -> "90 days", `P2W` -> "2 weeks", `PT36H` -> "36 hours". */
export function durationWords(iso: string): string | undefined {
  const m = /^P(?:(\d+)Y)?(?:(\d+)M)?(?:(\d+)W)?(?:(\d+)D)?(?:T(?:(\d+)H)?(?:(\d+)M)?)?$/.exec(
    iso.trim(),
  );
  if (!m || iso.trim() === 'P' || iso.trim().endsWith('T')) return undefined;
  const unit = ['year', 'month', 'week', 'day', 'hour', 'minute'];
  const parts = unit
    .map((u, i) => {
      const n = Number(m[i + 1] ?? 0);
      return n > 0 ? `${n} ${u}${n === 1 ? '' : 's'}` : '';
    })
    .filter(Boolean);
  return parts.length ? parts.join(' ') : undefined;
}
