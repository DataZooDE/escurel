/// Dev inspector panel: the gateway's `tools/list`, grouped by the
/// per-entry `scope` label (`agent` | `admin`). An agent token only
/// ever receives the agent subset, so the ADMIN section is simply
/// absent on an agent connection. Each row shows the tool name, its
/// scope badge, and the MCP execution-hint annotations
/// (read-only / destructive / idempotent) as badges.
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../client/errors.dart';
import '../client/models.dart';
import '../state/providers.dart';
import '../theme/app_theme.dart';

class ToolsPanel extends ConsumerWidget {
  const ToolsPanel({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final tools = ref.watch(toolsListProvider);
    return Semantics(
      label: 'tools-panel',
      container: true,
      explicitChildNodes: true,
      child: tools.when(
        loading: () =>
            const Center(child: CircularProgressIndicator(strokeWidth: 2)),
        error: (e, _) => Padding(
          padding: const EdgeInsets.all(16),
          child: Text(
            'error: ${humanizeEscurelError(e)}',
            style: Theme.of(context).textTheme.bodySmall?.copyWith(
              color: kError,
            ),
          ),
        ),
        data: (list) {
          final agent = list.where((t) => t.scope != 'admin').toList();
          final admin = list.where((t) => t.scope == 'admin').toList();
          return ListView(
            padding: const EdgeInsets.all(12),
            children: [
              _ScopeSection(title: 'AGENT', tools: agent),
              if (admin.isNotEmpty) ...[
                const SizedBox(height: 12),
                _ScopeSection(title: 'ADMIN', tools: admin),
              ],
            ],
          );
        },
      ),
    );
  }
}

class _ScopeSection extends StatelessWidget {
  const _ScopeSection({required this.title, required this.tools});

  final String title;
  final List<ToolInfo> tools;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.symmetric(vertical: 6),
          child: Text(
            '$title · ${tools.length}',
            style: text.labelSmall?.copyWith(color: kOutline, letterSpacing: 1),
          ),
        ),
        for (final t in tools) _ToolRow(tool: t),
        if (tools.isEmpty)
          Padding(
            padding: const EdgeInsets.symmetric(vertical: 4),
            child: Text(
              'no tools',
              style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
            ),
          ),
      ],
    );
  }
}

class _ToolRow extends StatelessWidget {
  const _ToolRow({required this.tool});

  final ToolInfo tool;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    return Semantics(
      label: 'tool-row:${tool.name}',
      container: true,
      explicitChildNodes: true,
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 3),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    tool.name,
                    style: text.bodySmall?.copyWith(
                      fontWeight: FontWeight.w600,
                    ),
                  ),
                  if (tool.description.isNotEmpty)
                    Text(
                      tool.description,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: text.labelSmall?.copyWith(
                        color: kOnSurfaceVariant,
                      ),
                    ),
                ],
              ),
            ),
            const SizedBox(width: 8),
            _ScopeBadge(scope: tool.scope),
            if (tool.readOnly == true) const _ExecBadge('read-only'),
            if (tool.destructive == true)
              const _ExecBadge('destructive', color: kError),
            if (tool.idempotent == true) const _ExecBadge('idempotent'),
          ],
        ),
      ),
    );
  }
}

class _ScopeBadge extends StatelessWidget {
  const _ScopeBadge({required this.scope});

  final String scope;

  @override
  Widget build(BuildContext context) {
    final admin = scope == 'admin';
    return Container(
      margin: const EdgeInsets.only(left: 4),
      padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
      decoration: BoxDecoration(
        color: admin ? kSecondaryContainer : kPrimary.withValues(alpha: 0.12),
        borderRadius: BorderRadius.circular(6),
      ),
      child: Text(
        scope.toUpperCase(),
        style: Theme.of(context).textTheme.labelSmall?.copyWith(
          fontSize: 9,
          fontWeight: FontWeight.w700,
          color: admin ? kOnSecondaryContainer : kPrimary,
        ),
      ),
    );
  }
}

class _ExecBadge extends StatelessWidget {
  const _ExecBadge(this.label, {this.color = kOnSurfaceVariant});

  final String label;
  final Color color;

  @override
  Widget build(BuildContext context) {
    return Container(
      margin: const EdgeInsets.only(left: 4),
      padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
      decoration: BoxDecoration(
        color: color.withValues(alpha: 0.1),
        borderRadius: BorderRadius.circular(6),
      ),
      child: Text(
        label,
        style: Theme.of(context).textTheme.labelSmall?.copyWith(
          fontSize: 9,
          fontWeight: FontWeight.w700,
          color: color,
        ),
      ),
    );
  }
}
