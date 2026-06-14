import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../config/feature_flags.dart';
import '../theme/app_theme.dart';

class StatusBar extends ConsumerWidget {
  const StatusBar({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    final success = Theme.of(context).explorerColors.success;
    final async = ref.watch(currentCapabilitiesProvider);

    final (Color dot, String label) = async.when(
      loading: () => (scheme.outlineVariant, 'backend: querying…'),
      error: (e, _) => (scheme.error, 'backend: error · $e'),
      data: (v) => (
        success,
        'backend: ${v.app} ${v.version} · ${v.capabilities.length} capabilities',
      ),
    );

    return Container(
      height: 24,
      color: scheme.surfaceContainerLow,
      padding: const EdgeInsets.symmetric(horizontal: 12),
      child: Row(
        children: [
          Container(
            width: 6,
            height: 6,
            decoration: BoxDecoration(color: dot, shape: BoxShape.circle),
          ),
          const SizedBox(width: 6),
          Text(
            label,
            key: const ValueKey('status_bar.backend'),
            style: text.labelSmall?.copyWith(color: scheme.onSurfaceVariant),
          ),
        ],
      ),
    );
  }
}
