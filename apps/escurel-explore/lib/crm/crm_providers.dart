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

/// Skills whose instances are *source artifacts* — the inbox feed.
/// These are the channels agents ingest from (email/meeting/doc), as
/// opposed to the CRM entities they promote into.
const kArtifactSkills = <String>['email', 'meeting', 'doc'];

/// One source artifact, projected from an instance's frontmatter for
/// the inbox row (title, source channel, timestamp, provenance badge).
class Artifact {
  const Artifact({
    required this.pageId,
    required this.skill,
    required this.title,
    required this.source,
    required this.at,
    required this.provenance,
  });

  final String pageId;
  final String skill;
  final String title;
  final String source;
  /// RFC 3339 timestamp from frontmatter `at`, or empty.
  final String at;
  /// `EXTRACTED` | `AUTO-PROMOTED` | '' (from frontmatter `provenance`).
  final String provenance;

  static Artifact fromInstance(InstanceSummary i) {
    final fm = i.frontmatter;
    String s(String k) => (fm[k] as String?) ?? '';
    final title = s('subject').isNotEmpty
        ? s('subject')
        : (s('title').isNotEmpty ? s('title') : _slug(i.id));
    return Artifact(
      pageId: i.id,
      skill: i.skill,
      title: title,
      source: s('source'),
      at: s('at'),
      provenance: s('provenance'),
    );
  }
}

String _slug(String pageId) {
  final base = pageId.split('/').last;
  final stem = base.endsWith('.md') ? base.substring(0, base.length - 3) : base;
  // `email__proposal` → `proposal`.
  final us = stem.indexOf('__');
  return us >= 0 ? stem.substring(us + 2) : stem;
}

/// The source inbox: every artifact instance across [kArtifactSkills],
/// newest first. Uses the real `list_instances` (ordered by `at`) and
/// merges the per-skill feeds into one timeline.
final inboxArtifactsProvider = FutureProvider<List<Artifact>>((ref) async {
  final client = ref.watch(escurelClientProvider);
  final asOf = ref.watch(asOfStringProvider);
  final available = await ref.watch(skillsCatalogueProvider.future);
  final ids = available.map((s) => s.id).toSet();
  final out = <Artifact>[];
  for (final skill in kArtifactSkills) {
    if (!ids.contains(skill)) continue;
    final rows = await client.listInstances(skill, orderBy: 'at desc', asOf: asOf);
    out.addAll(rows.map(Artifact.fromInstance));
  }
  // Newest first across the merged feed; undated sink to the bottom.
  out.sort((a, b) {
    if (a.at.isEmpty && b.at.isEmpty) return 0;
    if (a.at.isEmpty) return 1;
    if (b.at.isEmpty) return -1;
    return b.at.compareTo(a.at);
  });
  return out;
});

/// Both-direction typed neighbours of the focused entity — the source
/// data for the radial skill-wheel and the lineage rail. Empty when no
/// entity is focused.
final currentNeighboursProvider = FutureProvider<List<Neighbour>>((ref) async {
  final id = ref.watch(currentPageIdProvider);
  if (id == null) return const <Neighbour>[];
  final asOf = ref.watch(asOfStringProvider);
  return ref.watch(escurelClientProvider).neighbours(id, direction: LinkDirection.both, asOf: asOf);
});

/// The corpus's event time-span — the min/max `at` across all artifact
/// instances — used by the time scrubber to map slider position to an
/// `as_of` instant. Computed **without** an `as_of` cut so the scrubber's
/// own range never shrinks as you scrub. Null when no artifact is dated.
final corpusRangeProvider = FutureProvider<({DateTime start, DateTime end})?>((ref) async {
  final client = ref.watch(escurelClientProvider);
  final available = await ref.watch(skillsCatalogueProvider.future);
  final ids = available.map((s) => s.id).toSet();
  DateTime? lo;
  DateTime? hi;
  for (final skill in kArtifactSkills) {
    if (!ids.contains(skill)) continue;
    for (final i in await client.listInstances(skill)) {
      final at = DateTime.tryParse((i.frontmatter['at'] as String?) ?? '');
      if (at == null) continue;
      if (lo == null || at.isBefore(lo)) lo = at;
      if (hi == null || at.isAfter(hi)) hi = at;
    }
  }
  if (lo == null || hi == null || !lo.isBefore(hi)) return null;
  return (start: lo, end: hi);
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
