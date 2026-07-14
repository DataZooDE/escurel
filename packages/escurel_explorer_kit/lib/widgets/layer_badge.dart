import 'package:flutter/material.dart';

import '../theme/app_theme.dart';

/// Tiny pill calling out a skill imported from a subscribed pack
/// (`layer: base@<pack>@<version>`, REQ-LAYER-04) with a lock marker —
/// base skills are read-only at this node (the server rejects writes
/// with `layer_read_only`). Overlay skills (the default) render nothing:
/// tenant-authored pages are the unremarkable case.
///
/// Carries a stable `skill-layer:<id>` semantics label so the demo
/// verification (rodney) can assert the catalogue surfaces the pin at a
/// glance. The label is the selector contract — do not rename casually.
class LayerBadge extends StatelessWidget {
  const LayerBadge({super.key, required this.skillId, required this.layer});

  final String skillId;
  final String layer;

  @override
  Widget build(BuildContext context) {
    if (!layer.startsWith('base@')) return const SizedBox.shrink();
    // `base@logistics-midmarket@v7` → `logistics-midmarket@v7`.
    final pin = layer.substring('base@'.length);
    final text = Theme.of(context).textTheme;
    return Semantics(
      label: 'skill-layer:$skillId',
      identifier: 'skill-layer:$skillId',
      container: true,
      explicitChildNodes: true,
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
        decoration: BoxDecoration(
          color: kSurfaceContainerHighest,
          borderRadius: BorderRadius.circular(6),
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            const Icon(Icons.lock_outline, size: 9, color: kOnSurfaceVariant),
            const SizedBox(width: 3),
            Text(
              pin,
              style: text.labelSmall?.copyWith(
                color: kOnSurfaceVariant,
                fontSize: 9,
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// Tiny pill on a tenant overlay skill that SHADOWS a pack-imported
/// base skill (REQ-LAYER-03): shows the shadowed pin so operators see
/// "this is a specialisation of pack content" at a glance. Carries a
/// stable `skill-shadow:<id>` semantics label (the rodney selector
/// contract) — do not rename casually.
class ShadowBadge extends StatelessWidget {
  const ShadowBadge({super.key, required this.skillId, required this.shadows});

  final String skillId;

  /// The shadowed base pin, e.g. `base@logistics-midmarket@v7`.
  final String shadows;

  @override
  Widget build(BuildContext context) {
    final pin = shadows.startsWith('base@')
        ? shadows.substring('base@'.length)
        : shadows;
    final text = Theme.of(context).textTheme;
    return Semantics(
      label: 'skill-shadow:$skillId',
      identifier: 'skill-shadow:$skillId',
      container: true,
      explicitChildNodes: true,
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
        decoration: BoxDecoration(
          color: kSurfaceContainerHigh,
          borderRadius: BorderRadius.circular(6),
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            const Icon(Icons.layers_outlined, size: 9, color: kOnSurfaceVariant),
            const SizedBox(width: 3),
            Text(
              'shadows $pin',
              style: text.labelSmall?.copyWith(
                color: kOnSurfaceVariant,
                fontSize: 9,
              ),
            ),
          ],
        ),
      ),
    );
  }
}
