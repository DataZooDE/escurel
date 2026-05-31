/// Providers specific to the data-zoo / CRM workspace.
///
/// Reuses the shared seam (`escurelClientProvider`,
/// `skillsCatalogueProvider`, `currentPageIdProvider`) from
/// `state/providers.dart`; adds only what the CRM chrome needs on top.
library;

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../client/models.dart';
import '../md/frontmatter.dart';
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

/// The user's *explicit* event-pane collapse choice. `null` means "follow
/// the page-type default" (skills auto-minimize, instances show events); a
/// concrete bool is a chevron toggle that overrides that default. The
/// override is reset to `null` when the focused page flips skill↔instance
/// (see `_SplitBody`), so each context re-derives its default while the
/// chevron stays live — including re-opening the pane on a skill page.
final leftCollapsedProvider = StateProvider<bool?>((ref) => null);
final rightCollapsedProvider = StateProvider<bool>((ref) => false);

/// Whether the focused page is a skill (skills carry no events). Derived
/// reactively from the expanded current page.
final currentPageIsSkillProvider = Provider<bool>((ref) {
  final page = ref.watch(currentPageProvider).valueOrNull;
  return page?.pageType == PageType.skill;
});

/// The left event pane's *effective* collapsed state: the user's explicit
/// chevron choice ([leftCollapsedProvider]) when set, otherwise the
/// page-type default — skills carry no events, so they auto-minimize. The
/// override (not an OR over the skill flag) is what lets the chevron
/// re-open the pane on a skill page; `_SplitBody` clears it on a
/// skill↔instance transition so each context falls back to its default.
final effectiveLeftCollapsedProvider = Provider<bool>((ref) {
  final choice = ref.watch(leftCollapsedProvider);
  return choice ?? ref.watch(currentPageIsSkillProvider);
});

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

/// The selected event-type (SOURCES) filter — a single `label_skill`
/// (the processing skill an event links to), or null for all. Toggling
/// the active chip clears it.
final eventSourceFilterProvider = StateProvider<String?>((ref) => null);

/// The distinct `label_skill`s across the focused instance's event
/// history — the chips the SOURCES filter offers. Sorted, stable.
final availableSourcesProvider = Provider<List<String>>((ref) {
  final history = ref.watch(entityEventHistoryProvider).valueOrNull ?? const <Event>[];
  final set = <String>{};
  for (final e in history) {
    if (e.labelSkill.isNotEmpty) set.add(e.labelSkill);
  }
  final out = set.toList()..sort();
  return out;
});

/// The focused instance's event history up to the `as_of` cut (the
/// events that had landed by T) and matching the SOURCES filter.
/// Undated events always pass the time cut.
final entityEventsProvider = FutureProvider<List<Event>>((ref) async {
  final all = await ref.watch(entityEventHistoryProvider.future);
  final asOf = ref.watch(asOfProvider);
  final source = ref.watch(eventSourceFilterProvider);
  final cut = asOf?.toUtc();
  return all.where((e) {
    if (source != null && e.labelSkill != source) return false;
    if (cut == null) return true;
    final at = DateTime.tryParse(e.at ?? '');
    return at == null || !at.isAfter(cut);
  }).toList();
});

/// The inbox — unprocessed events across the tenant, newest first.
final inboxEventsProvider = FutureProvider<List<Event>>((ref) async {
  return ref.watch(escurelClientProvider).listInbox();
});

/// The focused instance's CRDT snapshot timeline — the discrete
/// `taken_at` points `expand(asOf=T)` can replay, oldest first. Powers
/// the instance view's version markers. Empty when the instance has no
/// recorded history.
final instanceSnapshotsProvider = FutureProvider<List<String>>((ref) async {
  final id = ref.watch(currentPageIdProvider);
  if (id == null) return const <String>[];
  return ref.watch(escurelClientProvider).listSnapshots(id);
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

/// Resolve a typed `[[skill::slug]]` reference to its page id and focus
/// it — used by the wheel/lineage nodes (neighbours return slugs, the
/// editor navigates by page id).
Future<void> focusWikilink(WidgetRef ref, String linkSkill, String slug) async {
  final scenario = ref.read(scenarioProvider);
  final resolved = await ref
      .read(escurelClientProvider)
      .resolve('[[$linkSkill::$slug]]', scenario: scenario);
  if (resolved.exists && resolved.pageId.isNotEmpty) {
    navigateToInstance(ref, resolved.pageId);
  }
}
