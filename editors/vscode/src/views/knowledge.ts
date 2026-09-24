import * as vscode from 'vscode';
import type { EscurelClient, Instance } from '../client';
import { uriForPage } from '../fs/provider';
import { log } from '../log';
import { describeError } from '../errors';
import { instanceRow, skillRow, type InstanceRow, type SkillRow } from './knowledgeModel';

type Node =
  | SkillRow
  | InstanceRow
  | { kind: 'more'; skill: string; cursor: string }
  | { kind: 'error'; message: string };

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
  private readonly pages = new Map<string, { rows: InstanceRow[]; cursor: string | null }>();

  constructor(private readonly client: () => EscurelClient) {}

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
    this.pages.clear();
    this.changed.fire(undefined);
  }

  getTreeItem(n: Node): vscode.TreeItem {
    switch (n.kind) {
      case 'skill': {
        const item = new vscode.TreeItem(n.label, vscode.TreeItemCollapsibleState.Collapsed);
        item.description = n.description;
        item.tooltip = new vscode.MarkdownString(
          `**${n.skill.id}** — ${n.skill.summary ?? n.skill.description}\n\n${n.readOnly ? '_read-only (' + n.skill.layer + ')_' : 'layer ' + n.skill.layer}`,
        );
        item.iconPath = new vscode.ThemeIcon(
          'symbol-class',
          new vscode.ThemeColor('charts.purple'),
        );
        item.contextValue = n.readOnly ? 'skill.readonly' : 'skill';
        item.resourceUri = uriForPage(`markdown/skills/${n.skill.id}.md`);
        return item;
      }
      case 'instance': {
        const item = new vscode.TreeItem(n.label, vscode.TreeItemCollapsibleState.None);
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
        const item = new vscode.TreeItem(n.message, vscode.TreeItemCollapsibleState.None);
        item.iconPath = new vscode.ThemeIcon('warning', new vscode.ThemeColor('errorForeground'));
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
        return skills.map(skillRow);
      }
      if (n.kind === 'skill') {
        const page = this.pages.get(n.skill.id) ?? (await this.fetch(n.skill.id, undefined));
        return this.rows(page);
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

  private async fetch(skill: string, cursor: string | undefined) {
    const res = await this.client().listInstancesPage({ skill_id: skill, limit: PAGE, cursor });
    const prev = this.pages.get(skill)?.rows ?? [];
    const page = {
      rows: [...prev, ...res.instances.map((i: Instance) => instanceRow(i))],
      cursor: res.next_cursor,
    };
    this.pages.set(skill, page);
    return page;
  }

  private async loadMore(skill: string, cursor: string): Promise<void> {
    await this.fetch(skill, cursor);
    this.changed.fire(undefined);
  }
}
