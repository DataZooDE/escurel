import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../crm/capture_bar.dart';
import '../crm/event_pane.dart';
import '../crm/group_members_pane.dart';
import '../editor/catalogue_pane.dart';
import '../editor/entity_editor.dart';
import '../editor/right_rail.dart';
import '../editor/webhook_deliveries_pane.dart';
import '../state/providers.dart';
import '../theme/app_theme.dart';
import 'status_bar.dart';
import 'topbar.dart';

/// The main editor surface (route `/`): Catalogue (left) | EntityEditor
/// (centre) | a tabbed right panel folding in the link graph, the event
/// view + inbox, the outbound webhook delivery log, and RBAC group
/// membership. The CaptureBar is pinned at the bottom (above the
/// StatusBar) so capturing a new event is always one field away.
class AppShell extends ConsumerWidget {
  const AppShell({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    return const Scaffold(
      appBar: Topbar(),
      body: Column(
        children: [
          Expanded(child: _WorkspaceRow()),
          CaptureBar(key: ValueKey('shell.capture_bar')),
          StatusBar(key: ValueKey('shell.status_bar')),
        ],
      ),
    );
  }
}

class _WorkspaceRow extends ConsumerWidget {
  const _WorkspaceRow();

  // Bounds keep a pane usable — it can't be dragged away or swallow the
  // centre editor.
  static const _leftMin = 180.0, _leftMax = 480.0;
  static const _rightMin = 220.0, _rightMax = 560.0;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    return LayoutBuilder(
      builder: (context, constraints) {
        if (constraints.maxWidth >= 900) {
          final leftW = ref.watch(leftPaneWidthProvider);
          final rightW = ref.watch(rightPaneWidthProvider);
          return Row(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              SizedBox(width: leftW, child: const CataloguePane()),
              // Drag right → wider catalogue.
              _ResizeHandle(
                semanticsLabel: 'resize-left',
                onDrag: (dx) => ref
                    .read(leftPaneWidthProvider.notifier)
                    .update((w) => (w + dx).clamp(_leftMin, _leftMax)),
              ),
              const Expanded(child: EntityEditor()),
              // Drag left → wider right rail (the rail is to its right).
              _ResizeHandle(
                semanticsLabel: 'resize-right',
                onDrag: (dx) => ref
                    .read(rightPaneWidthProvider.notifier)
                    .update((w) => (w - dx).clamp(_rightMin, _rightMax)),
              ),
              SizedBox(width: rightW, child: const _RightTabs()),
            ],
          );
        }
        return const Column(
          children: [
            Expanded(child: CataloguePane()),
            Divider(height: 1),
            Expanded(flex: 2, child: EntityEditor()),
            Divider(height: 1),
            Expanded(flex: 2, child: _RightTabs()),
          ],
        );
      },
    );
  }
}

/// A thin draggable divider between two panes: a 1px rule centred in an
/// 8px hit target, with a column-resize cursor. `onDrag` receives the
/// horizontal delta (px) of each drag update. The `resize-*` Semantics
/// label is the a11y/test contract.
class _ResizeHandle extends StatelessWidget {
  const _ResizeHandle({required this.semanticsLabel, required this.onDrag});

  final String semanticsLabel;
  final void Function(double dx) onDrag;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.resizeColumn,
      child: GestureDetector(
        behavior: HitTestBehavior.translucent,
        onHorizontalDragUpdate: (d) => onDrag(d.delta.dx),
        child: Semantics(
          label: semanticsLabel,
          child: const SizedBox(
            width: 8,
            child: Center(child: VerticalDivider(width: 1)),
          ),
        ),
      ),
    );
  }
}

/// The folded-in right panel: a four-tab controller surfacing the
/// link graph, the event view + inbox, the webhook delivery log, and
/// RBAC group membership. Each tab keeps its child's existing
/// Semantics labels; the tabs themselves carry stable `tab-*` labels.
class _RightTabs extends StatelessWidget {
  const _RightTabs();

  @override
  Widget build(BuildContext context) {
    return DefaultTabController(
      length: 4,
      child: Column(
        children: [
          const ColoredBox(
            color: kSurfaceContainerLow,
            child: TabBar(
              isScrollable: true,
              tabAlignment: TabAlignment.start,
              labelColor: kPrimary,
              unselectedLabelColor: kOnSurfaceVariant,
              indicatorColor: kPrimary,
              tabs: [
                _RailTab(label: 'tab-links', text: 'Links'),
                _RailTab(label: 'tab-events', text: 'Events'),
                _RailTab(label: 'tab-webhooks', text: 'Webhooks'),
                _RailTab(label: 'tab-members', text: 'Members'),
              ],
            ),
          ),
          const Divider(height: 1),
          Expanded(
            child: TabBarView(
              children: [
                const RightRail(),
                const EventPane(),
                const WebhookDeliveriesPane(),
                // GroupMembersPane sizes itself min-height; give it room
                // to scroll on short panels.
                SingleChildScrollView(
                  child: Container(
                    color: kSurfaceContainerLow,
                    child: const GroupMembersPane(),
                  ),
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

class _RailTab extends StatelessWidget {
  const _RailTab({required this.label, required this.text});

  final String label;
  final String text;

  @override
  Widget build(BuildContext context) {
    return Semantics(
      label: label,
      button: true,
      child: Tab(
        height: 40,
        child: Text(text, style: const TextStyle(fontSize: 13)),
      ),
    );
  }
}
