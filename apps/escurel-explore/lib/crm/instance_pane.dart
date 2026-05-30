/// RIGHT pane (M7) — the **instance view** of the focused memory: the
/// instance connected to its skills (the skill-wheel) over its
/// materialized state, plus its **state over time** — version markers
/// that jump the materialized state to each recorded CRDT snapshot
/// (`expand(asOf=T)`), complementing the continuous time scrubber.
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../editor/entity_editor.dart';
import '../state/providers.dart';
import '../theme/app_theme.dart';
import 'crm_providers.dart';
import 'skill_wheel.dart';

class InstancePane extends StatelessWidget {
  const InstancePane({super.key});

  @override
  Widget build(BuildContext context) {
    return Semantics(
      label: 'instance-pane',
      container: true,
      explicitChildNodes: true,
      child: const Column(
        children: [
          // The instance ↔ skills connection.
          SizedBox(height: 220, child: SkillWheel()),
          Divider(height: 1, color: kOutlineVariant),
          // State over time: discrete snapshot version markers.
          _VersionMarkers(),
          // The materialized state (re-materialized as-of T).
          Expanded(child: EntityEditor()),
        ],
      ),
    );
  }
}

/// The instance's state-over-time markers: a `now` chip plus one chip per
/// recorded CRDT snapshot. Tapping a version jumps the materialized state
/// to that snapshot (`asOf = taken_at`); `now` clears the cut. The active
/// chip reflects the snapshot at-or-before the current cut. Hidden when
/// the instance has no recorded history.
class _VersionMarkers extends ConsumerWidget {
  const _VersionMarkers();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final snaps = ref.watch(instanceSnapshotsProvider).valueOrNull ?? const <String>[];
    if (snaps.isEmpty) return const SizedBox.shrink();
    final asOf = ref.watch(asOfProvider);
    final text = Theme.of(context).textTheme;

    // Which version is active: the latest snapshot at-or-before the cut,
    // or `now` (index -1) when there is no cut / nothing reaches back.
    var activeIndex = -1;
    if (asOf != null) {
      final cut = asOf.toUtc();
      for (var i = 0; i < snaps.length; i++) {
        final t = DateTime.tryParse(snaps[i]);
        if (t != null && !t.isAfter(cut)) activeIndex = i;
      }
    }

    return Semantics(
      label: 'version-markers',
      container: true,
      explicitChildNodes: true,
      child: Container(
        width: double.infinity,
        color: kSurfaceContainerLow,
        padding: const EdgeInsets.fromLTRB(12, 8, 12, 8),
        child: Row(
          children: [
            Text('STATE OVER TIME', style: text.labelSmall?.copyWith(color: kOutline, letterSpacing: 1)),
            const SizedBox(width: 10),
            Expanded(
              child: Wrap(
                spacing: 6,
                runSpacing: 6,
                children: [
                  _VersionChip(
                    label: 'now',
                    semantics: 'version-now',
                    active: activeIndex == -1,
                    onTap: () => ref.read(asOfProvider.notifier).state = null,
                  ),
                  for (var i = 0; i < snaps.length; i++)
                    _VersionChip(
                      label: 'v${i + 1}',
                      semantics: 'version-v${i + 1}',
                      active: activeIndex == i,
                      onTap: () =>
                          ref.read(asOfProvider.notifier).state = DateTime.tryParse(snaps[i])?.toUtc(),
                    ),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }
}

class _VersionChip extends StatelessWidget {
  const _VersionChip({
    required this.label,
    required this.semantics,
    required this.active,
    required this.onTap,
  });
  final String label;
  final String semantics;
  final bool active;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return Semantics(
      label: semantics,
      button: true,
      selected: active,
      onTap: onTap,
      excludeSemantics: true,
      child: InkWell(
        onTap: onTap,
        borderRadius: BorderRadius.circular(999),
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 4),
          decoration: BoxDecoration(
            color: active ? kPrimary : kSurfaceContainer,
            borderRadius: BorderRadius.circular(999),
            border: Border.all(color: active ? kPrimary : kOutlineVariant),
          ),
          child: Text(
            label,
            style: Theme.of(context).textTheme.labelSmall?.copyWith(
                  color: active ? kSurface : kOnSurfaceVariant,
                  fontWeight: FontWeight.w600,
                ),
          ),
        ),
      ),
    );
  }
}
