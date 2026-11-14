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
    vscode.commands.registerCommand('escurel.agents.issueToken', (row?: AgentRow) =>
      issueToken(services, row),
    ),
    vscode.commands.registerCommand('escurel.agents.revokeTokens', async (row?: AgentRow) => {
      if (await revokeTokens(services, row)) await tree.load();
    }),
  );
  return tree;
}

/** The lifetimes offered for an external agent's token (the gateway allows 1 h .. 90 d). */
const TTL_CHOICES = [
  { label: '30 days', description: 'the default', secs: 30 * 86_400 },
  { label: '7 days', secs: 7 * 86_400 },
  { label: '90 days', description: 'the longest the gateway allows', secs: 90 * 86_400 },
  { label: '1 day', secs: 86_400 },
];

/**
 * "Issue token…" (#829): a bearer for an external agent (Claude Code, Codex, Multica) that runs as
 * this agent profile. The token is shown ONCE in a modal and copied only on request; it is never
 * written to a log, a setting or the secret store.
 */
async function issueToken(services: Services, row: AgentRow | undefined): Promise<void> {
  if (!row) return;
  const ttl = await vscode.window.showQuickPick(TTL_CHOICES, {
    title: `Issue a token for agent ${row.label}`,
    placeHolder: 'How long the token lives (revoke it any time with "Revoke tokens")',
  });
  if (!ttl) return;
  let issued;
  try {
    issued = await services.client.issueAgentToken({ agent: row.label, ttl_secs: ttl.secs });
  } catch (error) {
    void vscode.window.showErrorMessage(
      `Could not issue a token for agent ${row.label}: ${describeError(error)}`,
    );
    return;
  }
  const copy = 'Copy token';
  const choice = await vscode.window.showInformationMessage(
    `Token for agent ${row.label} (${issued.subject}), valid until ${issued.expires_at}.`,
    {
      modal: true,
      detail:
        "It is shown once and not stored. Copy it into the external agent's MCP configuration " +
        '(references/12). It can do only what the agent page allows.',
    },
    copy,
  );
  if (choice === copy) await vscode.env.clipboard.writeText(issued.token);
}

/**
 * "Revoke tokens" (#829): the gateway's `revoke_agent_tokens` moves the agent page's
 * `token_epoch` forward on its own clock, so every token issued so far — external and run tokens
 * — is refused from its next call on. Confirmed first. True when the tokens were revoked.
 */
async function revokeTokens(services: Services, row: AgentRow | undefined): Promise<boolean> {
  if (!row) return false;
  const revoke = 'Revoke tokens';
  const confirmed = await vscode.window.showWarningMessage(
    `Revoke every token issued so far for agent ${row.label}?`,
    {
      modal: true,
      detail:
        'External agents using them are refused from their next call. A run of this agent in ' +
        'flight loses its token too: the attempt under way fails, and a retry (when the run ' +
        'has attempts left) runs on a fresh token.',
    },
    revoke,
  );
  if (confirmed !== revoke) return false;
  try {
    await services.client.revokeAgentTokens(row.label);
    void vscode.window.showInformationMessage(
      `Tokens for agent ${row.label} are revoked; tokens issued from now on work.`,
    );
    return true;
  } catch (error) {
    void vscode.window.showErrorMessage(
      `Could not revoke the tokens of agent ${row.label}: ${describeError(error)}`,
    );
    return false;
  }
}
