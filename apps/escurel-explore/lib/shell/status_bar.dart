import 'package:flutter/material.dart';

import '../theme/app_theme.dart';

class StatusBar extends StatelessWidget {
  const StatusBar({super.key});

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    return Container(
      height: 24,
      color: scheme.surfaceContainerLow,
      padding: const EdgeInsets.symmetric(horizontal: 12),
      child: Row(
        children: [
          Container(width: 6, height: 6, decoration: const BoxDecoration(
            color: kOutlineVariant,
            shape: BoxShape.circle,
          )),
          const SizedBox(width: 6),
          Text(
            'backend: not connected',
            style: text.labelSmall?.copyWith(color: kOnSurfaceVariant),
          ),
        ],
      ),
    );
  }
}
