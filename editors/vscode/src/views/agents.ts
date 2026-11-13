import * as vscode from 'vscode';
import type { UsageReportResponse } from '../client/types';
import { describeError } from '../errors';
import type { Services } from '../services';
import { agentProfile, agentRows, type AgentProfile, type AgentRow } from './agentsModel';
import { LazyFlatTree, type FlatRead } from './lazyTree';
import { usageFailure, usageRange } from './usageModel';

/** The agent profile skill (a system skill, admin-write-only). */
const AGENT_SKILL = 'agent';
/** At most this many pages of profiles are read. */
const MAX_PAGES = 10;

/**
 * The Agents view (Runs side bar): every agent profile with its harness, model and scopes, and how
 * its runs went over the last 30 days (`usage_report {group_by: [agent]}`, admin only; anyone else
 * sees the profiles without the counts). A click opens the agent page.
 */
export class AgentsTree extends LazyFlatTree<AgentRow> {
  constructor(services: Services) {
    super(services, 'Open this view to list agents.');
  }

  protected async read(): Promise<FlatRead<AgentRow>> {
    const client = this.services.client;
    const profiles: AgentProfile[] = [];
    try {
      let pages = 0;
      for await (const page of client.listInstances({ skill_id: AGENT_SKILL, limit: 100 })) {
        profiles.push(...page.instances.map(agentProfile));
        if (++pages >= MAX_PAGES) break;
      }
    } catch (error) {
      return { rows: [], message: `Could not list agents: ${describeError(error)}` };
    }
    if (!profiles.length)
      return {
        rows: [],
        message: 'No agent profiles yet. An admin creates one as an instance of the agent skill.',
      };
    let usage: UsageReportResponse | 'admin-required' | undefined;
    try {
      usage = await client.usageReport({ group_by: ['agent'], ...usageRange('30d', new Date()) });
    } catch (error) {
      // The profiles are worth showing without their counts; only an admin may read the counts.
      usage = usageFailure(error).adminRequired ? 'admin-required' : undefined;
    }
    return {
      rows: agentRows(profiles, { usage, window: 'Last 30 days' }),
      message: usage === 'admin-required' ? 'Run counts need an admin sign-in.' : undefined,
    };
  }

  getTreeItem(row: AgentRow): vscode.TreeItem {
    const item = new vscode.TreeItem(row.label, vscode.TreeItemCollapsibleState.None);
    item.id = row.id;
    item.description = row.description;
    item.tooltip = row.tooltip;
    item.iconPath = new vscode.ThemeIcon('hubot');
    item.accessibilityInformation = { label: `Agent ${row.label}: ${row.description}` };
    item.contextValue = 'agent';
    item.command = { command: 'escurel.openPage', title: 'Open agent', arguments: [row.pageId] };
    return item;
  }
}

export function registerAgentsView(
  context: vscode.ExtensionContext,
  services: Services,
): AgentsTree {
  const tree = new AgentsTree(services);
  const view = vscode.window.createTreeView('escurel.agents', {
    treeDataProvider: tree,
    showCollapseAll: false,
  });
  tree.bindView(view);
  context.subscriptions.push(
    tree,
    view,
    vscode.commands.registerCommand('escurel.agents.refresh', () => tree.load()),
  );
  return tree;
}
