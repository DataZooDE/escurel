import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../config/feature_flags.dart';
import '../shell/status_bar.dart';
import '../shell/topbar.dart';
import '../theme/app_theme.dart';
import 'md_inspector_panel.dart';

/// The dev inspector drawer — surfaces escurel's primitives directly
/// for under-the-hood debugging. Today: a Markdown Inspector that
/// pastes text and shows the parsed frontmatter + wikilinks.
///
/// LaneStore + Index inspector panels arrive once admin MCP tools
/// are landed on escurel-server.
class InspectorShell extends ConsumerWidget {
  const InspectorShell({super.key, required this.panelId});

  final String panelId;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    return Scaffold(
      appBar: const Topbar(),
      body: Column(
        children: [
          Container(
            color: Theme.of(context).colorScheme.surfaceContainerLow,
            padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
            child: Row(
              children: [
                IconButton(
                  icon: const Icon(Icons.arrow_back, size: 18),
                  tooltip: 'Back to editor',
                  visualDensity: VisualDensity.compact,
                  onPressed: () => context.go('/'),
                ),
                const SizedBox(width: 4),
                Text(
                  'Dev Inspector — ${_label(panelId)}',
                  style: Theme.of(context).textTheme.titleSmall,
                ),
                const SizedBox(width: 12),
                _PanelChip(
                  id: 'md',
                  label: 'Markdown',
                  selected: panelId == 'md',
                ),
                const SizedBox(width: 4),
                _PanelChip(
                  id: 'lanes',
                  label: 'LaneStore',
                  selected: panelId == 'lanes',
                  disabled: !ref.watch(adminEnabledProvider),
                ),
                const SizedBox(width: 4),
                _PanelChip(
                  id: 'index',
                  label: 'Index',
                  selected: panelId == 'index',
                  disabled: !ref.watch(adminEnabledProvider),
                ),
              ],
            ),
          ),
          Expanded(child: _panelFor(panelId)),
          const StatusBar(),
        ],
      ),
    );
  }

  Widget _panelFor(String id) {
    return switch (id) {
      'md' => const MdInspectorPanel(),
      _ => _ComingSoonPanel(panelId: id),
    };
  }

  String _label(String id) => switch (id) {
        'md' => 'Markdown',
        'lanes' => 'LaneStore',
        'index' => 'Index',
        _ => id,
      };
}

class _PanelChip extends StatelessWidget {
  const _PanelChip({
    required this.id,
    required this.label,
    required this.selected,
    this.disabled = false,
  });

  final String id;
  final String label;
  final bool selected;
  final bool disabled;

  @override
  Widget build(BuildContext context) {
    final fg = disabled ? kOnSurfaceVariant : (selected ? Colors.white : kOnSurface);
    final bg = disabled
        ? kSurfaceContainer
        : (selected ? kPrimary : kSurfaceContainerHigh);
    final chip = Container(
      padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 4),
      decoration: BoxDecoration(color: bg, borderRadius: BorderRadius.circular(6)),
      child: Text(
        disabled ? '$label · admin' : label,
        style: Theme.of(context).textTheme.labelSmall?.copyWith(color: fg),
      ),
    );
    if (disabled) {
      return Tooltip(message: 'Requires admin MCP tools (M3+).', child: chip);
    }
    return InkWell(
      borderRadius: BorderRadius.circular(6),
      onTap: () => GoRouter.of(context).go('/inspector/$id'),
      child: chip,
    );
  }
}

class _ComingSoonPanel extends StatelessWidget {
  const _ComingSoonPanel({required this.panelId});

  final String panelId;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(32),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Text(panelId, style: text.titleMedium),
            const SizedBox(height: 4),
            Text(
              'This panel needs admin MCP tools on escurel-server.\nWill light up once the contract amendment lands.',
              textAlign: TextAlign.center,
              style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
            ),
          ],
        ),
      ),
    );
  }
}
