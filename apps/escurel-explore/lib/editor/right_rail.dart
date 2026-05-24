import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../state/providers.dart';
import '../theme/app_theme.dart';

/// Right pane — backlinks (incoming neighbours) for the current page.
///
/// Outgoing-links + neighbours graph follow in PR-5 (graphview);
/// for the editor merge gate, the backlinks list is enough to prove
/// the link-graph round trip works end-to-end.
class RightRail extends ConsumerWidget {
  const RightRail({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final async = ref.watch(currentBacklinksProvider);
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;

    return Container(
      key: const ValueKey('pane.right'),
      color: scheme.surfaceContainerLow,
      padding: const EdgeInsets.all(12),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text('Backlinks', style: text.titleSmall),
          const SizedBox(height: 8),
          Expanded(
            child: async.when(
              loading: () => const Center(child: CircularProgressIndicator(strokeWidth: 2)),
              error: (e, _) => Text('$e', style: const TextStyle(color: kError)),
              data: (links) {
                if (links.isEmpty) {
                  return Text(
                    'No backlinks',
                    style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
                  );
                }
                return ListView.separated(
                  key: const ValueKey('right_rail.backlinks'),
                  itemCount: links.length,
                  separatorBuilder: (_, _) => const SizedBox(height: 4),
                  itemBuilder: (context, i) {
                    final link = links[i];
                    return InkWell(
                      borderRadius: BorderRadius.circular(4),
                      onTap: () =>
                          ref.read(currentPageIdProvider.notifier).state = link.src,
                      child: Container(
                        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 6),
                        decoration: BoxDecoration(
                          color: kSurfaceContainerLowest,
                          border: Border.all(color: kOutlineVariant),
                          borderRadius: BorderRadius.circular(6),
                        ),
                        child: Row(
                          children: [
                            Expanded(child: Text(link.src, style: text.bodySmall)),
                            if (link.linkSkill.isNotEmpty)
                              Text(
                                link.linkSkill,
                                style: text.labelSmall?.copyWith(color: kOnSurfaceVariant),
                              ),
                          ],
                        ),
                      ),
                    );
                  },
                );
              },
            ),
          ),
        ],
      ),
    );
  }
}
