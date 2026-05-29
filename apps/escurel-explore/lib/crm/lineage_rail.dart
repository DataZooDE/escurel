/// Lineage / links rail — the typed neighbours of the focused entity
/// as a tappable list, grouped by link skill. Complements the radial
/// skill-wheel with a readable index of the same `neighbours` data
/// (the mock's "Lineage" + backlinks list).
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../client/models.dart';
import '../theme/app_theme.dart';
import 'crm_providers.dart';

class LineageRail extends ConsumerWidget {
  const LineageRail({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final text = Theme.of(context).textTheme;
    final neighbours = ref.watch(currentNeighboursProvider);

    return Semantics(
      label: 'lineage-rail',
      container: true,
      explicitChildNodes: true,
      child: Padding(
        padding: const EdgeInsets.fromLTRB(16, 12, 16, 12),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text('LINEAGE & LINKS', style: text.labelSmall?.copyWith(color: kOutline, letterSpacing: 1)),
            const SizedBox(height: 8),
            Expanded(
              child: neighbours.when(
                loading: () => const Center(child: CircularProgressIndicator(strokeWidth: 2)),
                error: (e, _) => Text('error: $e', style: text.bodySmall?.copyWith(color: kError)),
                data: (edges) {
                  final unique = <String, Neighbour>{};
                  for (final e in edges) {
                    if (e.dst.isEmpty) continue;
                    unique.putIfAbsent('${e.linkSkill}::${e.dst}', () => e);
                  }
                  if (unique.isEmpty) {
                    return Text('No typed links', style: text.bodySmall?.copyWith(color: kOutline));
                  }
                  final items = unique.values.toList()
                    ..sort((a, b) => a.linkSkill.compareTo(b.linkSkill));
                  return ListView(
                    children: [for (final n in items) _LinkTile(n: n)],
                  );
                },
              ),
            ),
          ],
        ),
      ),
    );
  }
}

class _LinkTile extends ConsumerWidget {
  const _LinkTile({required this.n});
  final Neighbour n;
  @override
  Widget build(BuildContext context, WidgetRef ref) {
    return Semantics(
      label: 'lineage-link',
      button: true,
      onTap: () => focusWikilink(ref, n.linkSkill, n.dst),
      excludeSemantics: true,
      child: InkWell(
        onTap: () => focusWikilink(ref, n.linkSkill, n.dst),
        borderRadius: BorderRadius.circular(8),
        child: Padding(
          padding: const EdgeInsets.symmetric(vertical: 6, horizontal: 4),
          child: Row(
            children: [
              _SkillChip(n.linkSkill),
              const SizedBox(width: 8),
              Expanded(
                child: Text(
                  n.dst,
                  style: Theme.of(context).textTheme.bodyMedium?.copyWith(color: kPrimary),
                  overflow: TextOverflow.ellipsis,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _SkillChip extends StatelessWidget {
  const _SkillChip(this.skill);
  final String skill;
  @override
  Widget build(BuildContext context) => Container(
        padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
        decoration: BoxDecoration(color: kSecondaryContainer, borderRadius: BorderRadius.circular(6)),
        child: Text(
          skill,
          style: Theme.of(context).textTheme.labelSmall?.copyWith(color: kOnSecondaryContainer, fontSize: 9),
        ),
      );
}
