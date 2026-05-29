/// Providers specific to the data-zoo / CRM workspace.
///
/// Reuses the shared seam (`escurelClientProvider`,
/// `skillsCatalogueProvider`, `currentPageIdProvider`) from
/// `state/providers.dart`; adds only what the CRM chrome needs on top.
library;

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../client/models.dart';
import '../state/providers.dart';

/// Every instance in the tenant, flattened across skills (skips the
/// `escurel` meta-skill, which has no instances). Powers the
/// breadcrumb's "Instances N" count and any cross-skill listing.
final allInstancesProvider = FutureProvider<List<InstanceSummary>>((ref) async {
  final client = ref.watch(escurelClientProvider);
  final skills = await ref.watch(skillsCatalogueProvider.future);
  final out = <InstanceSummary>[];
  for (final s in skills) {
    if (s.id == 'escurel') continue;
    out.addAll(await client.listInstances(s.id));
  }
  return out;
});

/// Both-direction typed neighbours of the focused entity — the source
/// data for the radial skill-wheel and the lineage rail. Empty when no
/// entity is focused.
final currentNeighboursProvider = FutureProvider<List<Neighbour>>((ref) async {
  final id = ref.watch(currentPageIdProvider);
  if (id == null) return const <Neighbour>[];
  return ref.watch(escurelClientProvider).neighbours(id, direction: LinkDirection.both);
});

/// Resolve a typed `[[skill::slug]]` reference to its page id and focus
/// it — used by the wheel/lineage nodes (neighbours return slugs, the
/// editor navigates by page id).
Future<void> focusWikilink(WidgetRef ref, String linkSkill, String slug) async {
  final resolved = await ref.read(escurelClientProvider).resolve('[[$linkSkill::$slug]]');
  if (resolved.exists && resolved.pageId.isNotEmpty) {
    ref.read(currentPageIdProvider.notifier).state = resolved.pageId;
  }
}
