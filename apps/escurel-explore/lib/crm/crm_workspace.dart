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

import '../editor/catalogue_pane.dart';
import '../editor/entity_editor.dart';
import '../editor/right_rail.dart';
import '../theme/app_theme.dart';
import 'command_bar.dart';
import 'crm_breadcrumb.dart';

class CrmWorkspace extends ConsumerWidget {
  const CrmWorkspace({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
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
              SizedBox(width: 300, child: _Region(label: 'navigator', child: CataloguePane())),
              VerticalDivider(width: 1),
              Expanded(child: _Region(label: 'entity', child: EntityEditor())),
              VerticalDivider(width: 1),
              SizedBox(width: 340, child: _Region(label: 'detail', child: RightRail())),
            ],
          );
        }
        return const Column(
          children: [
            Expanded(child: _Region(label: 'navigator', child: CataloguePane())),
            Divider(height: 1),
            Expanded(child: _Region(label: 'entity', child: EntityEditor())),
          ],
        );
      },
    );
  }
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
