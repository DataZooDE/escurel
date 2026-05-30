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

/// Layout state for the resizable/collapsible two-pane split: the left
/// pane's width fraction, and per-pane collapse flags.
final leftPaneFractionProvider = StateProvider<double>((ref) => 0.42);
final leftCollapsedProvider = StateProvider<bool>((ref) => false);
final rightCollapsedProvider = StateProvider<bool>((ref) => false);

/// The event currently open in the left detail pane. Distinct from the
/// pinned entity ([currentPageIdProvider], right pane) — opening an
/// event does NOT change the pinned entity (the two foci of the M7
/// workspace).
final openEventProvider = StateProvider<String?>((ref) => null);

/// The focused instance's *full* processed event history (`list_events`),
/// oldest first. Unfiltered — the timeline range + the cut both derive
/// from this so the range never shrinks as you scrub.
final entityEventHistoryProvider = FutureProvider<List<Event>>((ref) async {
  final id = ref.watch(currentPageIdProvider);
  if (id == null) return const <Event>[];
  return ref.watch(escurelClientProvider).listEvents(id);
});

/// The focused instance's event history up to the `as_of` cut (the
/// events that had landed by T). Undated events always show.
final entityEventsProvider = FutureProvider<List<Event>>((ref) async {
  final all = await ref.watch(entityEventHistoryProvider.future);
  final asOf = ref.watch(asOfProvider);
  if (asOf == null) return all;
  final cut = asOf.toUtc();
  return all.where((e) {
    final at = DateTime.tryParse(e.at ?? '');
    return at == null || !at.isAfter(cut);
  }).toList();
});

/// The inbox — unprocessed events across the tenant, newest first.
final inboxEventsProvider = FutureProvider<List<Event>>((ref) async {
  return ref.watch(escurelClientProvider).listInbox();
});

/// The currently-open event (left detail), looked up in the entity
/// history or the inbox. Null when nothing is open.
final openEventDetailProvider = Provider<Event?>((ref) {
  final id = ref.watch(openEventProvider);
  if (id == null) return null;
  final history = ref.watch(entityEventHistoryProvider).valueOrNull ?? const <Event>[];
  final inbox = ref.watch(inboxEventsProvider).valueOrNull ?? const <Event>[];
  for (final e in [...history, ...inbox]) {
    if (e.eventId == id) return e;
  }
  return null;
});

/// Both-direction typed neighbours of the focused entity — the source
/// data for the radial skill-wheel and the lineage rail. Empty when no
/// entity is focused.
final currentNeighboursProvider = FutureProvider<List<Neighbour>>((ref) async {
  final id = ref.watch(currentPageIdProvider);
  if (id == null) return const <Neighbour>[];
  final asOf = ref.watch(asOfStringProvider);
  final scenario = ref.watch(scenarioProvider);
  return ref
      .watch(escurelClientProvider)
      .neighbours(id, direction: LinkDirection.both, asOf: asOf, scenario: scenario);
});

/// The focused instance's event time-span — min/max `at` across its
/// full event history (+ inbox) — used by the time scrubber to map
/// slider position to an `as_of` instant. Computed from the *unfiltered*
/// history so the scrubber's range never shrinks as you scrub. Null when
/// nothing is dated.
final corpusRangeProvider = FutureProvider<({DateTime start, DateTime end})?>((ref) async {
  final history = await ref.watch(entityEventHistoryProvider.future);
  final inbox = await ref.watch(inboxEventsProvider.future);
  DateTime? lo;
  DateTime? hi;
  for (final e in [...history, ...inbox]) {
    final at = DateTime.tryParse(e.at ?? '');
    if (at == null) continue;
    if (lo == null || at.isBefore(lo)) lo = at;
    if (hi == null || at.isAfter(hi)) hi = at;
  }
  if (lo == null || hi == null || !lo.isBefore(hi)) return null;
  return (start: lo, end: hi);
});

/// Resolve a typed `[[skill::slug]]` reference to its page id and focus
/// it — used by the wheel/lineage nodes (neighbours return slugs, the
/// editor navigates by page id).
Future<void> focusWikilink(WidgetRef ref, String linkSkill, String slug) async {
  final scenario = ref.read(scenarioProvider);
  final resolved = await ref
      .read(escurelClientProvider)
      .resolve('[[$linkSkill::$slug]]', scenario: scenario);
  if (resolved.exists && resolved.pageId.isNotEmpty) {
    ref.read(currentPageIdProvider.notifier).state = resolved.pageId;
  }
}
