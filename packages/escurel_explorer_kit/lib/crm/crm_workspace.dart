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

class CrmWorkspace extends ConsumerStatefulWidget {
  const CrmWorkspace({super.key});
  @override
  ConsumerState<CrmWorkspace> createState() => _CrmWorkspaceState();
}

class _CrmWorkspaceState extends ConsumerState<CrmWorkspace> {
  bool _autoFocused = false;

  @override
  Widget build(BuildContext context) {
    // Land on a populated view: auto-focus the engagement spine with
    // real processed event history (see autoFocusTargetProvider) once on
    // first load.
    if (!_autoFocused && ref.watch(currentPageIdProvider) == null) {
      ref.watch(autoFocusTargetProvider).whenData((target) {
        if (_autoFocused || target == null) return;
        _autoFocused = true;
        WidgetsBinding.instance.addPostFrameCallback((_) {
          if (mounted) ref.read(currentPageIdProvider.notifier).state = target;
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
          // Time-travel lives in the instance view's STATE OVER TIME
          // version markers; no separate bottom scrubber.
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
    // Clear the explicit collapse choice when the focused page flips
    // skill↔instance, so each context re-derives its default (skills
    // minimize, instances show events) while the chevron stays live.
    ref.listen<bool>(currentPageIsSkillProvider, (prev, next) {
      if (prev != next) {
        ref.read(leftCollapsedProvider.notifier).state = null;
      }
    });

    // Effective collapse: auto-minimized while a skill is focused (no
    // events), else the user's explicit chevron choice.
    final leftCollapsed = ref.watch(effectiveLeftCollapsedProvider);
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
      // The whole collapsed rail is the expand target. A centered 16px icon
      // is far too small to reliably hit — and when *both* panes collapse
      // the right rail balloons to a wide blank area — so an InkWell fills
      // the region and re-expands on a tap anywhere within it.
      return Semantics(
        label: label,
        container: true,
        explicitChildNodes: true,
        child: Semantics(
          label: '$label-expand',
          button: true,
          onTap: onToggle,
          excludeSemantics: true,
          child: Tooltip(
            message: 'Expand',
            child: InkWell(
              onTap: onToggle,
              // Whole rail stays tappable, but the chevron sits in the
              // same top 28px band as the collapse chevron (consistent
              // placement) rather than floating at the vertical centre.
              child: Align(
                alignment: Alignment.topCenter,
                child: SizedBox(
                  height: 28,
                  child: Center(
                    child: Icon(
                      edge == _Edge.right ? Icons.chevron_right : Icons.chevron_left,
                      size: 16,
                      color: kOnSurfaceVariant,
                    ),
                  ),
                ),
              ),
            ),
          ),
        ),
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
