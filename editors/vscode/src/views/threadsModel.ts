import type { NodeTarget, ThreadNode, ThreadView } from '../shared/protocol';

export interface OutlineRow {
  id: string;
  label: string;
  description: string;
  contextValue: 'escurel.run' | 'escurel.event' | 'escurel.changeset' | 'escurel.draft';
  collapsibleState: 'none' | 'collapsed' | 'expanded';
  target: NodeTarget;
  children: OutlineRow[];
}

/** Keep descendants in the model so VS Code can expand a collapsed row on demand. */
export function outlineRows(view: ThreadView, collapsed: ReadonlySet<string>): OutlineRow[] {
  const byId = new Map(view.nodes.map((node) => [node.id, node]));
  const seen = new Set<string>();
  function visit(node: ThreadNode): OutlineRow {
    seen.add(node.id);
    const children = node.children.flatMap((id) => {
      const child = byId.get(id);
      return child && !seen.has(id) ? [visit(child)] : [];
    });
    return {
      id: node.id,
      label: node.title,
      description: node.chips.map((chip) => chip.text).join(' · '),
      contextValue: `escurel.${node.kind}`,
      collapsibleState: children.length
        ? collapsed.has(node.id)
          ? 'collapsed'
          : 'expanded'
        : 'none',
      target: node.target,
      children,
    };
  }
  const root = byId.get(view.rootEventId);
  return root ? [visit(root)] : [];
}
