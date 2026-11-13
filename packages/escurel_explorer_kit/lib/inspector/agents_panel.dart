/// Dev inspector panel: the tenant's agent profiles (`agent` instances,
/// #823) — harness, model and scopes — with each one's last 30 days of
/// runs and success rate from `usage_report{group_by:[agent]}`. The
/// profiles are readable by anyone; the usage is admin-only, so a member
/// sees the profiles with "usage: admin only" instead of a refusal.
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../client/errors.dart';
import '../client/models.dart';
import '../state/providers.dart';
import '../state/usage.dart';
import '../theme/app_theme.dart';

/// The `agent` system skill's id.
const _agentSkill = 'agent';

final agentProfilesProvider = FutureProvider<List<AgentProfile>>((ref) async {
  final page = await ref
      .watch(escurelClientProvider)
      .listInstances(_agentSkill);
  return page.instances.map(AgentProfile.fromInstance).toList()
    ..sort((a, b) => a.id.compareTo(b.id));
});

class AgentsPanel extends ConsumerWidget {
  const AgentsPanel({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final profiles = ref.watch(agentProfilesProvider);
    // A failed or refused report reads like a non-admin's: no figures.
    final usage = ref.watch(usageByProvider('agent'));
    return Semantics(
      label: 'agents-panel',
      container: true,
      explicitChildNodes: true,
      child: profiles.when(
        loading: () =>
            const Center(child: CircularProgressIndicator(strokeWidth: 2)),
        error: (e, _) => Padding(
          padding: const EdgeInsets.all(16),
          child: Text(
            'error: ${humanizeEscurelError(e)}',
            style: Theme.of(
              context,
            ).textTheme.bodySmall?.copyWith(color: kError),
          ),
        ),
        data: (list) {
          final text = Theme.of(context).textTheme;
          final byAgent = usage.asData?.value;
          return ListView(
            padding: const EdgeInsets.all(12),
            children: [
              Padding(
                padding: const EdgeInsets.symmetric(vertical: 6),
                child: Text(
                  'AGENT PROFILES · ${list.length}',
                  style: text.labelSmall?.copyWith(
                    color: kOutline,
                    letterSpacing: 1,
                  ),
                ),
              ),
              if (list.isEmpty)
                Text(
                  'no agent profiles',
                  style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
                ),
              for (final a in list)
                _AgentRow(
                  agent: a,
                  usageLine: _usageLine(usage.isLoading, byAgent, a.principal),
                ),
            ],
          );
        },
      ),
    );
  }

  static String _usageLine(
    bool loading,
    Map<String, UsageRow>? byAgent,
    String principal,
  ) {
    if (loading) return 'usage: …';
    if (byAgent == null) return 'usage: admin only';
    final row = byAgent[principal];
    if (row == null || row.runs == 0) return 'no runs ($usageWindowLabel)';
    final rate = ((row.successRate ?? 0) * 100).round();
    return '${formatRuns(row.runs)} · $rate% ok · ${formatCost(row)}';
  }
}

class _AgentRow extends StatelessWidget {
  const _AgentRow({required this.agent, required this.usageLine});

  final AgentProfile agent;
  final String usageLine;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    final muted = text.labelSmall?.copyWith(color: kOnSurfaceVariant);
    return Semantics(
      label: 'agent-row:${agent.id}',
      container: true,
      explicitChildNodes: true,
      child: Container(
        margin: const EdgeInsets.symmetric(vertical: 3),
        padding: const EdgeInsets.all(8),
        decoration: BoxDecoration(
          color: kSurfaceContainerLow,
          borderRadius: BorderRadius.circular(6),
          border: Border.all(color: kOutlineVariant),
        ),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    agent.id,
                    style: text.bodySmall?.copyWith(
                      fontWeight: FontWeight.w600,
                    ),
                  ),
                  Text(
                    '${agent.harness ?? 'default harness'} · '
                    '${agent.model ?? 'default model'}',
                    style: muted,
                  ),
                  Text(_scopesSummary(agent), style: muted),
                  if (agent.skills.isNotEmpty)
                    Text(
                      'runs: ${agent.skills.join(', ')}',
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: muted,
                    ),
                ],
              ),
            ),
            const SizedBox(width: 8),
            Text(
              usageLine,
              style: text.labelSmall?.copyWith(fontWeight: FontWeight.w600),
            ),
          ],
        ),
      ),
    );
  }
}

/// "tools: search, expand, update_page +1 · write: markdown/instances/note/*"
/// — an absent scope is "any": scopes only narrow, never widen.
String _scopesSummary(AgentProfile a) {
  String list(List<String>? v, int shown) {
    if (v == null) return 'any';
    if (v.isEmpty) return 'none';
    final head = v.take(shown).join(', ');
    return v.length > shown ? '$head +${v.length - shown}' : head;
  }

  return 'tools: ${list(a.tools, 3)} · write: ${list(a.write, 2)}';
}
