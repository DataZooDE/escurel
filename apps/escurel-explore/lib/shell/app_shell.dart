import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../editor/catalogue_pane.dart';
import '../editor/entity_editor.dart';
import '../editor/right_rail.dart';
import 'status_bar.dart';
import 'topbar.dart';

class AppShell extends ConsumerWidget {
  const AppShell({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    return const Scaffold(
      appBar: Topbar(),
      body: Column(
        children: [
          Expanded(child: _WorkspaceRow()),
          StatusBar(key: ValueKey('shell.status_bar')),
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
        if (constraints.maxWidth >= 900) {
          return const Row(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              SizedBox(width: 280, child: CataloguePane()),
              VerticalDivider(width: 1),
              Expanded(child: EntityEditor()),
              VerticalDivider(width: 1),
              SizedBox(width: 320, child: RightRail()),
            ],
          );
        }
        return const Column(
          children: [
            Expanded(child: CataloguePane()),
            Divider(height: 1),
            Expanded(child: EntityEditor()),
            Divider(height: 1),
            Expanded(child: RightRail()),
          ],
        );
      },
    );
  }
}
