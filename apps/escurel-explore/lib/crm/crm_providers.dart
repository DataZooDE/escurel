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
