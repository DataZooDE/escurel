import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../theme/app_theme.dart';
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
              SizedBox(width: 280, child: _PanePlaceholder(
                key: ValueKey('pane.catalogue'),
                title: 'Catalogue',
                hint: 'Skills + instances will appear here in PR-4.',
              )),
              VerticalDivider(width: 1),
              Expanded(child: _PanePlaceholder(
                key: ValueKey('pane.editor'),
                title: 'Entity editor',
                hint: 'Open a page from the catalogue.',
              )),
              VerticalDivider(width: 1),
              SizedBox(width: 320, child: _PanePlaceholder(
                key: ValueKey('pane.right'),
                title: 'Backlinks',
                hint: 'Backlinks, outgoing links, and neighbours land in PR-4.',
              )),
            ],
          );
        }
        return const Column(
          children: [
            Expanded(child: _PanePlaceholder(
              key: ValueKey('pane.catalogue'),
              title: 'Catalogue',
              hint: 'Use a wider window for the full workspace.',
            )),
            Divider(height: 1),
            Expanded(child: _PanePlaceholder(
              key: ValueKey('pane.editor'),
              title: 'Entity editor',
              hint: '',
            )),
            Divider(height: 1),
            Expanded(child: _PanePlaceholder(
              key: ValueKey('pane.right'),
              title: 'Backlinks',
              hint: '',
            )),
          ],
        );
      },
    );
  }
}

class _PanePlaceholder extends StatelessWidget {
  const _PanePlaceholder({super.key, required this.title, required this.hint});

  final String title;
  final String hint;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    return Container(
      color: scheme.surfaceContainerLow,
      padding: const EdgeInsets.all(16),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(title, style: text.titleSmall),
          const SizedBox(height: 6),
          Text(
            hint,
            style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
          ),
        ],
      ),
    );
  }
}
