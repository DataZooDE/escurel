/// Scenario A/B/C switch from the mockup. Picks the active what-if
/// overlay (`Base` = the shared timeline). Selecting one sets the global
/// [scenarioProvider]; every scenario-aware read (the inbox, wikilink
/// resolution) re-fetches against that overlay — backed by the real
/// scenario column + QUALIFY override (PR-9). No faking.
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../state/providers.dart';
import '../theme/app_theme.dart';

/// (label, value) — `null` value is the shared base.
const _options = <(String, String?)>[
  ('Base', null),
  ('A', 'A'),
  ('B', 'B'),
  ('C', 'C'),
];

class ScenarioSwitch extends ConsumerWidget {
  const ScenarioSwitch({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final text = Theme.of(context).textTheme;
    final active = ref.watch(scenarioProvider);
    return Semantics(
      label: 'scenario-switch',
      container: true,
      explicitChildNodes: true,
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Text('SCENARIO', style: text.labelSmall?.copyWith(color: kOutline, letterSpacing: 1)),
          const SizedBox(width: 8),
          for (final (label, value) in _options)
            _ScenarioChip(
              label: label,
              value: value,
              selected: active == value,
              onTap: () => ref.read(scenarioProvider.notifier).state = value,
            ),
        ],
      ),
    );
  }
}

class _ScenarioChip extends StatelessWidget {
  const _ScenarioChip({
    required this.label,
    required this.value,
    required this.selected,
    required this.onTap,
  });
  final String label;
  final String? value;
  final bool selected;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 2),
      child: Semantics(
        label: 'scenario-${value ?? 'base'}',
        button: true,
        selected: selected,
        onTap: onTap,
        excludeSemantics: true,
        child: InkWell(
          borderRadius: BorderRadius.circular(6),
          onTap: onTap,
          child: Container(
            padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 5),
            decoration: BoxDecoration(
              color: selected ? kPrimary : kSurfaceContainerHigh,
              borderRadius: BorderRadius.circular(6),
            ),
            child: Text(
              label,
              style: text.labelMedium?.copyWith(
                color: selected ? Colors.white : kOnSurfaceVariant,
                fontWeight: FontWeight.w700,
              ),
            ),
          ),
        ),
      ),
    );
  }
}
