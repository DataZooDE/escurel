// What to say when a record has no thread or run to open. A row of a SQL table, a REST API or an MCP
// tool is read from its source: no agent wrote it, so "no run has changed this page yet" reads like a
// fault. Say what is true instead, and where the runs that DID read it are.

const SOURCE_NAMES: Record<string, string> = {
  sql_view: 'a SQL table',
  openapi: 'a REST API',
  mcp: 'an MCP tool',
  document: 'an uploaded document',
};

export function noThreadMessage(backendKind: string | undefined): string {
  const source = backendKind ? SOURCE_NAMES[backendKind] : undefined;
  if (source) {
    return `This record comes from ${source}, so no agent has written it and it has no thread. Runs that only read it are under Runs for this record.`;
  }
  return 'No agent has changed this page yet, so there is no thread to open. Runs that read it, if any, are under Runs for this record.';
}

/** The sentence under a record page when it has no thread; empty when there is one or nothing to say. */
export function noThreadNote(hasSource: boolean, hasThread: boolean): string {
  if (hasThread || !hasSource) return '';
  return 'This record comes from a source: no agent wrote it, so it has no thread. Runs that read it are under Runs for this record.';
}
