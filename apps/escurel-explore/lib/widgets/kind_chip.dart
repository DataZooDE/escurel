import 'package:flutter/material.dart';

import '../md/frontmatter.dart' as md;
import '../theme/app_theme.dart';

/// Tiny pill that calls out a page's kind (skill vs instance).
class KindChip extends StatelessWidget {
  const KindChip({super.key, required this.pageType});

  final md.PageType pageType;

  @override
  Widget build(BuildContext context) {
    final (bg, fg, label) = switch (pageType) {
      md.PageType.skill => (kSecondaryContainer, kOnSecondaryContainer, 'skill'),
      md.PageType.instance => (kSurfaceContainerHigh, kPrimary, 'instance'),
    };
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
      decoration: BoxDecoration(color: bg, borderRadius: BorderRadius.circular(6)),
      child: Text(
        label,
        style: Theme.of(context).textTheme.labelSmall?.copyWith(color: fg, fontSize: 9),
      ),
    );
  }
}
