/// The data-zoo / CRM workspace — the product surface from the
/// mockups. Top breadcrumb, a three-region body (left navigator ·
/// centre entity · right detail), and a bottom command bar.
///
/// PR-3 (this) is the shell: it composes the breadcrumb + command bar
/// around the existing catalogue / entity / backlinks widgets so the
/// seeded corpus is navigable end-to-end. Later PRs replace the right
/// region with the radial skill-wheel + lineage (PR-4) and the left
/// region with the source inbox + artifact reader (PR-6).
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../editor/entity_editor.dart';
import '../theme/app_theme.dart';
import '../state/providers.dart';
import 'command_bar.dart';
import 'crm_breadcrumb.dart';
import 'crm_providers.dart';
import 'inbox.dart';
import 'lineage_rail.dart';
import 'skill_wheel.dart';

class CrmWorkspace extends ConsumerStatefulWidget {
  const CrmWorkspace({super.key});
  @override
  ConsumerState<CrmWorkspace> createState() => _CrmWorkspaceState();
}

class _CrmWorkspaceState extends ConsumerState<CrmWorkspace> {
  bool _autoFocused = false;

  @override
  Widget build(BuildContext context) {
    // Land on a populated view (like the mock) — auto-focus the
    // engagement spine, else the first instance, once on first load.
    if (!_autoFocused && ref.watch(currentPageIdProvider) == null) {
      ref.watch(allInstancesProvider).whenData((all) {
        if (_autoFocused || all.isEmpty) return;
        _autoFocused = true;
        final pick = all.firstWhere(
          (i) => i.skill == 'engagement' && i.id.contains('spine'),
          orElse: () => all.first,
        );
        // InstanceSummary.id is the page_id (the open handle) — focus
        // it directly; no wikilink resolution needed.
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
          Expanded(child: _WorkspaceRow()),
          CommandBar(),
        ],
      ),
    );
  }
}

class _WorkspaceRow extends StatelessWidget {
  const _WorkspaceRow();

  @override
  Widget build(BuildContext context) {
    return LayoutBuilder(
      builder: (context, constraints) {
        // Wide: three columns (navigator · entity · detail). Narrow:
        // navigator over a stacked entity/detail.
        if (constraints.maxWidth >= 1000) {
          return const Row(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              SizedBox(width: 300, child: _Region(label: 'inbox', child: InboxList())),
              VerticalDivider(width: 1),
              Expanded(child: _Region(label: 'reader', child: EntityEditor())),
              VerticalDivider(width: 1),
              SizedBox(width: 360, child: _Region(label: 'detail', child: _DetailColumn())),
            ],
          );
        }
        return const Column(
          children: [
            Expanded(child: _Region(label: 'inbox', child: InboxList())),
            Divider(height: 1),
            Expanded(child: _Region(label: 'reader', child: EntityEditor())),
          ],
        );
      },
    );
  }
}

/// The right detail column: the radial skill-wheel over the lineage /
/// links rail. Both read the focused entity's `neighbours`.
class _DetailColumn extends StatelessWidget {
  const _DetailColumn();
  @override
  Widget build(BuildContext context) => const Column(
        children: [
          Expanded(flex: 5, child: SkillWheel()),
          Divider(height: 1),
          Expanded(flex: 4, child: LineageRail()),
        ],
      );
}

/// Names a workspace region in the semantics tree so the rodney
/// presence harness can assert the layout.
class _Region extends StatelessWidget {
  const _Region({required this.label, required this.child});
  final String label;
  final Widget child;
  @override
  Widget build(BuildContext context) => Semantics(
        label: 'region-$label',
        container: true,
        explicitChildNodes: true,
        child: child,
      );
}
