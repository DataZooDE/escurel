import type { TreeNode } from './skillTree';

/** What a skill's gate means for a change, in plain words (the bare word 'review' means nothing). */
export function autonomyMeaning(autonomy: string | undefined): string {
  switch (autonomy) {
    case 'auto':
      return 'changes are applied without review';
    case 'confirm':
      return 'the agent asks you to confirm before it acts';
    default:
      return 'changes need your approval';
  }
}

/** Where a skill's data lives, for a tooltip. */
export function backendMeaning(kind: string): string {
  switch (kind) {
    case 'sql_view':
      return 'a SQL view (read-only)';
    case 'openapi':
      return 'a REST service';
    case 'mcp':
      return 'an MCP server';
    case 'document':
      return 'uploaded documents';
    case 'markdown':
      return 'markdown pages in this knowledge base';
    default:
      return `a ${kind} source`;
  }
}

/** A codicon for a skill whose data lives outside the knowledge base; `undefined` keeps the role icon. */
export function backendIcon(kind: string): string | undefined {
  switch (kind) {
    case 'openapi':
      return 'cloud';
    case 'mcp':
      return 'plug';
    case 'sql_view':
      return 'table';
    case 'document':
      return 'file-text';
    default:
      return undefined;
  }
}

/** How many skills sit below a folder, at any depth. */
export function countSkills(node: TreeNode): number {
  if (node.kind === 'skill') return 1;
  return node.children.reduce((n, c) => n + countSkills(c), 0);
}
