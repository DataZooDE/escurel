/// The data-zoo / CRM workspace (M7) — two views of one memory.
///
/// `search` pinned top; a resizable + collapsible split of the LEFT
/// event view (event history + inbox) and the RIGHT instance view
/// (skill-wheel + materialized state); the time scrubber + scenario
/// switch; and `capture` pinned bottom. Two foci: the pinned entity
/// (`currentPageIdProvider`, right) and the open event
/// (`openEventProvider`, left) — opening an event does not move the entity.
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../theme/app_theme.dart';
import '../state/providers.dart';
import 'capture_bar.dart';
import 'crm_breadcrumb.dart';
import 'crm_providers.dart';
import 'event_pane.dart';
import 'instance_pane.dart';
import 'search_bar.dart';
import 'time_scrubber.dart';

class CrmWorkspace extends ConsumerStatefulWidget {
  const CrmWorkspace({super.key});
  @override
  ConsumerState<CrmWorkspace> createState() => _CrmWorkspaceState();
}

class _CrmWorkspaceState extends ConsumerState<CrmWorkspace> {
  bool _autoFocused = false;

  @override
  Widget build(BuildContext context) {
    // Land on a populated view: auto-focus the engagement spine (else
    // the first instance) once on first load.
    if (!_autoFocused && ref.watch(currentPageIdProvider) == null) {
      ref.watch(allInstancesProvider).whenData((all) {
        if (_autoFocused || all.isEmpty) return;
        _autoFocused = true;
        final pick = all.firstWhere(
          (i) => i.skill == 'engagement' && i.id.contains('spine'),
          orElse: () => all.first,
        );
        WidgetsBinding.instance.addPostFrameCallback((_) {
          if (mounted) ref.read(currentPageIdProvider.notifier).state = pick.id;
        });
      });
    }

    return const Scaffold(
      backgroundColor: kSurface,
      appBar: CrmBreadcrumb(),
      body: Column(
        children: [
          WorkspaceSearchBar(),
          Expanded(child: _SplitBody()),
          TimeScrubber(),
          CaptureBar(),
        ],
      ),
    );
  }
}

/// The resizable + collapsible two-pane body. The left pane's width is
/// a fraction of the available width, adjustable by dragging the divider;
/// either pane can collapse to a thin rail with an expand toggle.
class _SplitBody extends ConsumerWidget {
  const _SplitBody();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final leftCollapsed = ref.watch(leftCollapsedProvider);
    final rightCollapsed = ref.watch(rightCollapsedProvider);
    final fraction = ref.watch(leftPaneFractionProvider);

    return LayoutBuilder(
      builder: (context, c) {
        final w = c.maxWidth;
        const rail = 28.0;
        const divW = 7.0; // must match _DragDivider's width
        double leftW;
        if (leftCollapsed && !rightCollapsed) {
          leftW = rail;
        } else if (rightCollapsed && !leftCollapsed) {
          leftW = w - rail - divW;
        } else if (leftCollapsed && rightCollapsed) {
          leftW = rail;
        } else {
          leftW = (w * fraction).clamp(220.0, w - 320.0 - divW);
        }
        final rightW = w - leftW - divW;

        return Row(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            SizedBox(
              width: leftW,
              child: _CollapsibleRegion(
                label: 'region-events',
                collapsed: leftCollapsed,
                onToggle: () => ref.read(leftCollapsedProvider.notifier).state = !leftCollapsed,
                edge: _Edge.right,
                child: const EventPane(),
              ),
            ),
            _DragDivider(
              enabled: !leftCollapsed && !rightCollapsed,
              onDelta: (dx) {
                final next = ((leftW + dx) / w).clamp(0.2, 0.75);
                ref.read(leftPaneFractionProvider.notifier).state = next;
              },
            ),
            SizedBox(
              width: rightW,
              child: _CollapsibleRegion(
                label: 'region-instance',
                collapsed: rightCollapsed,
                onToggle: () => ref.read(rightCollapsedProvider.notifier).state = !rightCollapsed,
                edge: _Edge.left,
                child: const InstancePane(),
              ),
            ),
          ],
        );
      },
    );
  }
}

enum _Edge { left, right }

class _CollapsibleRegion extends StatelessWidget {
  const _CollapsibleRegion({
    required this.label,
    required this.collapsed,
    required this.onToggle,
    required this.edge,
    required this.child,
  });
  final String label;
  final bool collapsed;
  final VoidCallback onToggle;
  final _Edge edge;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    final toggle = Semantics(
      label: collapsed ? '$label-expand' : '$label-collapse',
      button: true,
      onTap: onToggle,
      excludeSemantics: true,
      child: IconButton(
        iconSize: 16,
        visualDensity: VisualDensity.compact,
        onPressed: onToggle,
        icon: Icon(
          collapsed
              ? (edge == _Edge.right ? Icons.chevron_right : Icons.chevron_left)
              : (edge == _Edge.right ? Icons.chevron_left : Icons.chevron_right),
        ),
      ),
    );

    if (collapsed) {
      return Semantics(
        label: label,
        container: true,
        explicitChildNodes: true,
        child: Center(child: toggle),
      );
    }
    return Semantics(
      label: label,
      container: true,
      explicitChildNodes: true,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          SizedBox(
            height: 28,
            child: Align(
              alignment: edge == _Edge.right ? Alignment.centerRight : Alignment.centerLeft,
              child: toggle,
            ),
          ),
          Expanded(child: child),
        ],
      ),
    );
  }
}

class _DragDivider extends StatelessWidget {
  const _DragDivider({required this.enabled, required this.onDelta});
  final bool enabled;
  final void Function(double dx) onDelta;
  @override
  Widget build(BuildContext context) {
    final bar = SizedBox(
      width: 7,
      child: Center(child: Container(width: 1, color: kOutlineVariant)),
    );
    if (!enabled) return bar;
    return MouseRegion(
      cursor: SystemMouseCursors.resizeColumn,
      child: GestureDetector(
        behavior: HitTestBehavior.translucent,
        onHorizontalDragUpdate: (d) => onDelta(d.delta.dx),
        child: Semantics(label: 'pane-resize', child: bar),
      ),
    );
  }
}
