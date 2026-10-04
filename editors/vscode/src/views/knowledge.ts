import { errorRowSpec } from './errorRow';
import { InstancePager } from './instancePager';
import * as vscode from 'vscode';
import type { EscurelClient, Instance } from '../client';
import { uriForPage } from '../fs/provider';
import { log } from '../log';
import { describeError } from '../errors';
import { instanceRow, type InstanceRow, type SkillRow } from './knowledgeModel';
import {
  ROLE_ICONS,
  buildSkillTree,
  effectiveRole,
  skillAccessibleName,
  type FolderRow,
} from './skillTree';

type Node =
  | FolderRow
  | SkillRow
  | InstanceRow
  | { kind: 'more'; skill: string; cursor: string }
  | { kind: 'error'; message: string; detail?: string };

const PAGE = 100;

/**
 * Knowledge (SPEC §3.1): Skills → Instances, instances loaded through the
 * `list_instances` cursor a page at a time behind a "Load more…" node.
 * Nothing is cached across refreshes (online only); a failed fetch shows
 * as one error row and the empty state's Reconnect (viewsWelcome).
 */
export class KnowledgeTree implements vscode.TreeDataProvider<Node> {
  private readonly changed = new vscode.EventEmitter<Node | undefined>();
  readonly onDidChangeTreeData = this.changed.event;
  private readonly pager: InstancePager<InstanceRow>;

  constructor(private readonly client: () => EscurelClient) {
    this.pager = new InstancePager<InstanceRow>(
      async (skill, cursor) => {
        const res = await this.client().listInstancesPage({ skill_id: skill, limit: PAGE, cursor });
        return { rows: res.instances.map((i: Instance) => instanceRow(i)), next: res.next_cursor };
      },
      (row) => row.pageId,
    );
  }

  static register(context: vscode.ExtensionContext, client: () => EscurelClient): KnowledgeTree {
    const tree = new KnowledgeTree(client);
    context.subscriptions.push(
      vscode.window.createTreeView('escurel.knowledge', {
        treeDataProvider: tree,
        showCollapseAll: true,
      }),
    );
    context.subscriptions.push(
      vscode.commands.registerCommand(
        'escurel.knowledge.loadMore',
        (n: { skill: string; cursor: string }) => tree.loadMore(n.skill, n.cursor),
      ),
    );
    return tree;
  }

  refresh(): void {
    // Retires every fetch in flight: a slow old result must not repopulate the cache.
    this.pager.reset();
    this.changed.fire(undefined);
  }

  getTreeItem(n: Node): vscode.TreeItem {
    switch (n.kind) {
      case 'folder': {
        const item = new vscode.TreeItem(
          n.label,
          n.collapsed
            ? vscode.TreeItemCollapsibleState.Collapsed
            : vscode.TreeItemCollapsibleState.Expanded,
        );
        item.id = `folder:${n.path}`;
        item.iconPath = new vscode.ThemeIcon('folder');
        item.tooltip = n.path;
        item.contextValue = 'folder';
        item.accessibilityInformation = { label: `folder ${n.path}`, role: 'treeitem' };
        return item;
      }
      case 'skill': {
        const { role, inferred } = effectiveRole(n.skill);
        const item = new vscode.TreeItem(n.label, vscode.TreeItemCollapsibleState.Collapsed);
        // A stable id: VS Code matches rows across a refresh by it, and keeps them expanded.
        item.id = `skill:${n.skill.id}`;
        item.description = n.description;
        const where = n.skill.folder ? `\n\nfolder \`${n.skill.folder}\`` : '';
        const tags = n.skill.tags?.length ? `\n\ntags: ${n.skill.tags.join(', ')}` : '';
        item.tooltip = new vscode.MarkdownString(
          `**${n.skill.title ?? n.skill.id}** — ${n.skill.summary ?? n.skill.description}\n\nrole **${role}**${inferred ? ' (inferred)' : ''} · ${n.readOnly ? '_read-only (' + n.skill.layer + ')_' : 'layer ' + n.skill.layer}${where}${tags}`,
        );
        item.iconPath = new vscode.ThemeIcon(
          ROLE_ICONS[role],
          new vscode.ThemeColor('charts.purple'),
        );
        item.contextValue = n.readOnly ? 'skill.readonly' : 'skill';
        item.resourceUri = uriForPage(`markdown/skills/${n.skill.id}.md`);
        item.accessibilityInformation = {
          label: skillAccessibleName(n.skill),
          role: 'treeitem',
        };
        return item;
      }
      case 'instance': {
        const item = new vscode.TreeItem(n.label, vscode.TreeItemCollapsibleState.None);
        item.id = `instance:${n.pageId}`;
        item.description = n.description;
        item.iconPath = new vscode.ThemeIcon('symbol-field', new vscode.ThemeColor('charts.blue'));
        item.contextValue = 'instance';
        item.resourceUri = uriForPage(n.pageId);
        item.command = {
          command: 'escurel.openInstance',
          title: 'Open instance',
          arguments: [n.pageId],
        };
        return item;
      }
      case 'more': {
        const item = new vscode.TreeItem('Load more…', vscode.TreeItemCollapsibleState.None);
        item.iconPath = new vscode.ThemeIcon('ellipsis');
        item.command = {
          command: 'escurel.knowledge.loadMore',
          title: 'Load more',
          arguments: [n],
        };
        return item;
      }
      case 'error': {
        const spec = errorRowSpec(n.message, n.detail);
        const item = new vscode.TreeItem(spec.label, vscode.TreeItemCollapsibleState.None);
        item.iconPath = new vscode.ThemeIcon('warning', new vscode.ThemeColor('errorForeground'));
        item.tooltip = spec.tooltip;
        item.command = { command: spec.command, title: 'Try again' };
        item.contextValue = 'error';
        return item;
      }
    }
  }

  async getChildren(n?: Node): Promise<Node[]> {
    try {
      if (!n) {
        const skills = await this.client().listSkills();
        await vscode.commands.executeCommand('setContext', 'escurel.connected', true);
        return buildSkillTree(skills);
      }
      if (n.kind === 'folder') return n.children;
      if (n.kind === 'skill') {
        try {
          return this.rows(await this.pager.first(n.skill.id));
        } catch (e) {
          // A failure to list ONE skill's rows is a row under that skill, not the whole tree's failure.
          log().warn(`escurel: knowledge tree: instances of ${n.skill.id}: ${describeError(e)}`);
          return [{ kind: 'error', message: "Couldn't load instances.", detail: describeError(e) }];
        }
      }
      return [];
    } catch (e) {
      log().warn(`escurel: knowledge tree: ${describeError(e)}`);
      if (!n) await vscode.commands.executeCommand('setContext', 'escurel.connected', false);
      return [{ kind: 'error', message: describeError(e) }];
    }
  }

  private rows(page: { rows: InstanceRow[]; cursor: string | null }): Node[] {
    const out: Node[] = [...page.rows];
    if (page.cursor)
      out.push({ kind: 'more', skill: page.rows[0]?.skill ?? '', cursor: page.cursor });
    return out;
  }

  private async loadMore(skill: string, cursor: string): Promise<void> {
    try {
      await this.pager.more(skill, cursor);
    } catch (e) {
      // A 401 or 429 here used to escape as an unhandled command rejection.
      log().warn(`escurel: knowledge tree: load more of ${skill}: ${describeError(e)}`);
      void vscode.window.showErrorMessage("Couldn't load more instances. Try again in a moment.");
    }
    this.changed.fire(undefined);
  }
}
