import 'package:flutter/material.dart';

import '../theme/app_theme.dart';

/// Tiny pill calling out a skill's *external* backend (`sql_view` /
/// `document`) and, when read-only, a lock marker. Native `markdown`
/// backends render nothing — they're the unremarkable default.
///
/// Carries a stable `skill-backend:<id>` semantics label so the demo
/// verification (rodney) can assert the catalogue surfaces the backend at
/// a glance. The label is the selector contract — do not rename casually.
class BackendBadge extends StatelessWidget {
  const BackendBadge({
    super.key,
    required this.skillId,
    required this.backendKind,
    this.writable = true,
  });

  final String skillId;
  final String backendKind;
  final bool writable;

  @override
  Widget build(BuildContext context) {
    if (backendKind == 'markdown') return const SizedBox.shrink();
    final (bg, fg, label) = switch (backendKind) {
      'sql_view' => (kPrimaryContainer, kOnPrimaryContainer, 'sql view'),
      'document' => (kSecondaryContainer, kOnSecondaryContainer, 'document'),
      _ => (kSurfaceContainerHighest, kOnSurfaceVariant, backendKind),
    };
    final text = Theme.of(context).textTheme;
    return Semantics(
      label: 'skill-backend:$skillId',
      identifier: 'skill-backend:$skillId',
      container: true,
      explicitChildNodes: true,
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
        decoration: BoxDecoration(
          color: bg,
          borderRadius: BorderRadius.circular(6),
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            if (!writable) ...[
              Icon(Icons.lock_outline, size: 9, color: fg),
              const SizedBox(width: 3),
            ],
            Text(
              label,
              style: text.labelSmall?.copyWith(color: fg, fontSize: 9),
            ),
          ],
        ),
      ),
    );
  }
}
