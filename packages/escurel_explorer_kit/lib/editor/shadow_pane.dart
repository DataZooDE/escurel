import 'package:flutter/material.dart';

import '../client/models.dart';
import '../theme/app_theme.dart';

/// Renders the *shadow-drift* section of a shadowing overlay skill page
/// (REQ-LAYER-03): the pack pin the shadowed base was imported under, plus
/// a compact table of the base's frontmatter with the fields the overlay
/// overrides (overlay value != base value) marked as drift. Pages without
/// a shadow render nothing (mirrors [BackendPane]'s self-hiding pattern).
///
/// This is the drift-visibility surface: an operator sees at a glance
/// which firm-authored values the tenant's specialisation departs from —
/// the base value stays visible, never silently masked.
///
/// Stable semantics labels (`shadow-pane`, `shadow-drift:<field>`) are
/// the rodney selector contract — do not rename casually.
class ShadowPane extends StatelessWidget {
  const ShadowPane({super.key, required this.page});

  final ExpandResult page;

  @override
  Widget build(BuildContext context) {
    final shadow = page.shadow;
    if (shadow == null) return const SizedBox.shrink();

    return Semantics(
      label: 'shadow-pane',
      identifier: 'shadow-pane',
      explicitChildNodes: true,
      child: Container(
        key: const ValueKey('entity_editor.shadow'),
        margin: const EdgeInsets.only(top: 16),
        padding: const EdgeInsets.all(12),
        decoration: BoxDecoration(
          color: kSurfaceContainerLow,
          borderRadius: BorderRadius.circular(8),
          border: Border.all(color: kOutlineVariant),
        ),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            _Header(pack: shadow.pack),
            const SizedBox(height: 10),
            _BaseFieldsTable(base: shadow.base, overlay: page.frontmatter),
          ],
        ),
      ),
    );
  }
}

class _Header extends StatelessWidget {
  const _Header({required this.pack});

  /// The shadowed base's pin, e.g. `base@logistics-midmarket@v7`.
  final String pack;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    final pin = pack.startsWith('base@')
        ? pack.substring('base@'.length)
        : pack;
    return Row(
      children: [
        const Icon(Icons.layers_outlined, size: 14, color: kOnSurfaceVariant),
        const SizedBox(width: 6),
        Text(
          'Shadows base',
          style: text.labelLarge?.copyWith(color: kOnSurfaceVariant),
        ),
        const SizedBox(width: 8),
        Expanded(
          child: Text(
            pin,
            style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
            overflow: TextOverflow.ellipsis,
          ),
        ),
      ],
    );
  }
}

/// One row per base frontmatter field: the field name, the base value,
/// and — when the overlay carries the same key with a different value —
/// a drift mark plus the overriding overlay value.
class _BaseFieldsTable extends StatelessWidget {
  const _BaseFieldsTable({required this.base, required this.overlay});

  final Map<String, dynamic> base;
  final Map<String, dynamic> overlay;

  /// The overlay overrides a base field iff it carries the key with a
  /// different (stringified) value. An absent overlay key is *not* drift —
  /// nothing was explicitly overridden.
  bool _drifts(String key) =>
      overlay.containsKey(key) && '${overlay[key]}' != '${base[key]}';

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    final entries = base.entries.toList();
    if (entries.isEmpty) {
      return Text(
        'base carries no frontmatter fields',
        style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
      );
    }
    return Column(
      children: [
        for (final e in entries)
          Padding(
            padding: const EdgeInsets.symmetric(vertical: 2),
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                SizedBox(
                  width: 160,
                  child: Text(
                    e.key,
                    style: text.labelMedium?.copyWith(color: kOnSurfaceVariant),
                  ),
                ),
                Expanded(child: Text('${e.value}', style: text.bodySmall)),
                if (_drifts(e.key))
                  _DriftMark(field: e.key, overlayValue: overlay[e.key]),
              ],
            ),
          ),
      ],
    );
  }
}

/// The drift pill on an overridden field: the overlay's value, marked so
/// operators see the departure from the base at a glance. Carries a stable
/// `shadow-drift:<field>` semantics label (the rodney selector contract).
class _DriftMark extends StatelessWidget {
  const _DriftMark({required this.field, required this.overlayValue});

  final String field;
  final dynamic overlayValue;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    return Semantics(
      label: 'shadow-drift:$field',
      identifier: 'shadow-drift:$field',
      container: true,
      explicitChildNodes: true,
      child: Container(
        margin: const EdgeInsets.only(left: 8),
        padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
        decoration: BoxDecoration(
          color: kSecondaryContainer,
          borderRadius: BorderRadius.circular(6),
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            const Icon(
              Icons.difference_outlined,
              size: 10,
              color: kOnSecondaryContainer,
            ),
            const SizedBox(width: 4),
            Text(
              'overlay: $overlayValue',
              style: text.labelSmall?.copyWith(
                color: kOnSecondaryContainer,
                fontSize: 9,
              ),
            ),
          ],
        ),
      ),
    );
  }
}
