/// Riverpod providers wiring the editor to an [EscurelClient].
///
/// Two seam points:
///
/// - [escurelClientProvider] returns the active client. The default
///   implementation builds a fixture client over a tiny inline
///   corpus so the app boots on its own. Tests and the
///   `dz-escurel-explore-fixture` Nomad job override this with a
///   richer fixture; PR-6 swaps it for [HttpEscurelClient] when the
///   build defines `ESCUREL_EXPLORE_MODE=http`.
///
/// - [currentPageIdProvider] holds the page id the editor is
///   focused on. Updates come from the catalogue, a wikilink-pill
///   tap, or a deep-link route (PR-5).
library;

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../client/escurel_client.dart';
import '../client/fixture_escurel_client.dart';
import '../client/http_escurel_client.dart';
import '../client/models.dart';
import '../config/env.dart';
import '../config/feature_flags.dart';

/// The single source of truth for which backend the editor speaks to.
///
/// Selects [HttpEscurelClient] when `ESCUREL_EXPLORE_MODE=http` and
/// a non-empty base URL is provided; otherwise falls back to the
/// inline-fixture client so the app boots standalone. Override in
/// tests via
/// `ProviderScope(overrides: [escurelClientProvider.overrideWithValue(...)])`.
final escurelClientProvider = Provider<EscurelClient>((ref) {
  final env = ref.watch(envProvider);
  final client = switch (env.mode) {
    // In HTTP mode, an explicit base URL wins; otherwise talk to the
    // origin we were served from. This is what makes the gateway's
    // `ESCUREL_SERVE_DEMO_DIR` bundle hit its own `/mcp` with no
    // build-time URL baked in (CLAUDE.md: demo runs as one process
    // alongside `/mcp`).
    AppMode.http => HttpEscurelClient(
      baseUrl: env.baseUrl.isNotEmpty ? env.baseUrl : Uri.base.origin,
      bearerToken: env.auth == AuthMode.bearer ? null : null,
    ),
    AppMode.fixture => _bootstrapInlineFixture(),
  };
  ref.onDispose(client.close);
  return client;
});

/// Currently focused page id. Null when the editor has nothing
/// opened (initial state).
final currentPageIdProvider = StateProvider<String?>((ref) => null);

/// Back-stack of previously-focused page ids. Pushed when following an
/// instance link (a wikilink pill, a skill-wheel / lineage node); popped
/// by the instance view's Back button. A fresh `search` clears it.
final navBackStackProvider = StateProvider<List<String>>((ref) => const []);

/// Focus an instance, recording the current one on the back-stack so a
/// Back action can return to it. No-op when the target is empty or
/// already focused. This is the single entry point every in-app
/// instance-link navigation routes through.
void navigateToInstance(WidgetRef ref, String pageId) {
  if (pageId.isEmpty) return;
  final current = ref.read(currentPageIdProvider);
  if (current == pageId) return;
  if (current != null && current.isNotEmpty) {
    ref.read(navBackStackProvider.notifier).state = [
      ...ref.read(navBackStackProvider),
      current,
    ];
  }
  ref.read(currentPageIdProvider.notifier).state = pageId;
}

/// Pop the back-stack, returning focus to the previous instance. Returns
/// false (no-op) when there is nothing to go back to.
bool navigateBack(WidgetRef ref) {
  final stack = ref.read(navBackStackProvider);
  if (stack.isEmpty) return false;
  ref.read(navBackStackProvider.notifier).state = stack.sublist(
    0,
    stack.length - 1,
  );
  ref.read(currentPageIdProvider.notifier).state = stack.last;
  return true;
}

/// Jump focus to a specific depth in the back-stack (a breadcrumb-trail
/// click): focus `stack[index]` and truncate the history to everything
/// before it, so the trail to the left of the clicked crumb is preserved
/// and everything to its right is dropped. No-op for an out-of-range index.
void navigateToDepth(WidgetRef ref, int index) {
  final stack = ref.read(navBackStackProvider);
  if (index < 0 || index >= stack.length) return;
  ref.read(navBackStackProvider.notifier).state = stack.sublist(0, index);
  ref.read(currentPageIdProvider.notifier).state = stack[index];
}

/// Drop the navigation history (used when a fresh search jumps the focus
/// to an unrelated instance, so Back doesn't wander a stale trail).
void clearNavHistory(WidgetRef ref) {
  if (ref.read(navBackStackProvider).isNotEmpty) {
    ref.read(navBackStackProvider.notifier).state = const [];
  }
}

/// Focus the skill *page* behind an instance or event — the durable "how"
/// the reference is an occurrence of. A skill resolves from its bare id
/// (`[[customer]]` → `markdown/skills/customer.md`), not `[[skill::id]]`.
/// Routes through [navigateToInstance] so the jump records the back-stack
/// exactly like an instance jump. No-op on an empty or dangling id.
Future<void> focusSkill(WidgetRef ref, String skillId) async {
  if (skillId.isEmpty) return;
  final scenario = ref.read(scenarioProvider);
  final resolved = await ref
      .read(escurelClientProvider)
      .resolve('[[$skillId]]', scenario: scenario);
  if (resolved.exists && resolved.pageId.isNotEmpty) {
    navigateToInstance(ref, resolved.pageId);
  }
}

/// Global time-travel cut. `null` = the present (no cut); otherwise the
/// time scrubber's selected instant. Every read provider passes it down
/// as the backend `as_of`, so scrubbing reshapes the whole workspace at
/// once with no per-widget plumbing.
final asOfProvider = StateProvider<DateTime?>((ref) => null);

/// The [asOfProvider] cut rendered as the RFC 3339 string the backend
/// expects, or null when at the present.
final asOfStringProvider = Provider<String?>((ref) {
  final at = ref.watch(asOfProvider);
  return at?.toUtc().toIso8601String();
});

/// Active what-if scenario overlay. `null` = the shared base timeline;
/// a value (e.g. "A"/"B"/"C") is passed to every scenario-aware read so
/// the scenario switch reshapes the projection without per-widget wiring.
final scenarioProvider = StateProvider<String?>((ref) => null);

/// Catalogue (skills + their instance counts).
final skillsCatalogueProvider = FutureProvider<List<SkillSummary>>((ref) {
  return ref.watch(escurelClientProvider).listSkills();
});

/// Instances of a given skill, keyed by skill id.
final instancesProvider = FutureProvider.family<List<InstanceSummary>, String>((
  ref,
  skillId,
) {
  return ref.watch(escurelClientProvider).listInstances(skillId);
});

/// The expanded current page, or null if nothing focused. Time-travels
/// with [asOfStringProvider]: a page born after the cut comes back with
/// an empty `pageId`, which the reader renders as a "not yet" placeholder.
final currentPageProvider = FutureProvider<ExpandResult?>((ref) async {
  final id = ref.watch(currentPageIdProvider);
  if (id == null) return null;
  final asOf = ref.watch(asOfStringProvider);
  final scenario = ref.watch(scenarioProvider);
  final page = await ref
      .watch(escurelClientProvider)
      .expand(id, asOf: asOf, scenario: scenario);
  // A time-cut page comes back with an empty pageId — treat it as "not
  // focused" so the reader falls back to its empty state.
  return page.pageId.isEmpty ? null : page;
});

/// Backlinks (incoming neighbours) for the current page.
final currentBacklinksProvider = FutureProvider<List<Neighbour>>((ref) async {
  final id = ref.watch(currentPageIdProvider);
  if (id == null) return const <Neighbour>[];
  final asOf = ref.watch(asOfStringProvider);
  final scenario = ref.watch(scenarioProvider);
  return ref
      .watch(escurelClientProvider)
      .neighbours(
        id,
        direction: LinkDirection.incoming,
        asOf: asOf,
        scenario: scenario,
      );
});

/// Outgoing links for the current page. The server returns directionless
/// edges, so backlinks vs outgoing are two separate `neighbours` calls.
final currentOutgoingLinksProvider = FutureProvider<List<Neighbour>>((
  ref,
) async {
  final id = ref.watch(currentPageIdProvider);
  if (id == null) return const <Neighbour>[];
  final asOf = ref.watch(asOfStringProvider);
  final scenario = ref.watch(scenarioProvider);
  return ref
      .watch(escurelClientProvider)
      .neighbours(
        id,
        direction: LinkDirection.outgoing,
        asOf: asOf,
        scenario: scenario,
      );
});

// ── editing (feat/explorer-editing) ──────────────────────────────

/// Whether a skill is operator-editable through the explorer: write
/// tools are enabled AND the skill exists AND it is ownerless (no
/// `owner_field`). Owner-bound skills (e.g. `private_profile`) are never
/// editable here regardless of the write capability. The function is
/// resolved against the catalogue snapshot; a not-yet-loaded catalogue
/// reads as not-editable (fail-closed for the write surface).
final skillEditableProvider = Provider<bool Function(String skillId)>((ref) {
  final writeEnabled = ref.watch(writeEnabledProvider);
  final catalogue = ref.watch(skillsCatalogueProvider);
  return (skillId) {
    if (!writeEnabled) return false;
    final skills = catalogue.asData?.value;
    if (skills == null) return false;
    final match = skills.where((s) => s.id == skillId);
    if (match.isEmpty) return false;
    return match.first.ownerField == null;
  };
});

/// The skill id of the current page (from the expanded page's `skill`),
/// or null when nothing is focused or the page hasn't loaded.
final currentPageSkillProvider = Provider<String?>((ref) {
  return ref.watch(currentPageProvider).asData?.value?.skill;
});

/// Whether the current page is editable (its skill is editable per
/// [skillEditableProvider]). Drives the "Bearbeiten" affordance.
final currentPageEditableProvider = Provider<bool>((ref) {
  final skill = ref.watch(currentPageSkillProvider);
  if (skill == null) return false;
  return ref.watch(skillEditableProvider)(skill);
});

/// Whether the editor is in edit mode for the current page. Reset to
/// false whenever the focused page changes (see [EntityEditor]).
final editModeProvider = StateProvider<bool>((ref) => false);

/// The in-progress edit of the current page: a mutable frontmatter map
/// (string/list values) plus the body text. Null when not editing.
class PageDraft {
  PageDraft({required this.frontmatter, required this.body});

  /// Editable frontmatter values, keyed by field name. Values are the
  /// raw strings shown in the form inputs; list fields are carried as
  /// `List<String>`.
  final Map<String, dynamic> frontmatter;
  String body;

  PageDraft copy() => PageDraft(
        frontmatter: {...frontmatter},
        body: body,
      );
}

/// The current page draft, or null when no edit is in progress.
final pageDraftProvider = StateProvider<PageDraft?>((ref) => null);

/// The latest validation issues for the in-progress draft (empty until
/// a validate runs). Held imperatively so a debounced/Save validate can
/// push results without a derived future re-running on every keystroke.
final pageValidationProvider = StateProvider<List<Issue>>((ref) => const []);

/// In-flight save state for the edit footer.
enum SaveStatus { idle, saving, error }

class SaveState {
  const SaveState({this.status = SaveStatus.idle, this.message});
  final SaveStatus status;
  final String? message;

  static const idle = SaveState();
}

final pageSaveProvider = StateProvider<SaveState>((ref) => SaveState.idle);

/// The inline boot corpus is intentionally small — two skills + two
/// instances — just enough for the app to render *something* on a
/// cold load when no overrides are in place. The richer
/// `examples/crm-demo/` corpus is wired in by main() in fixture
/// mode (PR-7 deployment work) and by tests directly.
FixtureEscurelClient _bootstrapInlineFixture() {
  return FixtureEscurelClient.fromSources(
    skillFiles: const {
      'note.md': '''---
type: skill
id: note
description: A free-form note. The simplest skill — useful for first-light demos.
required_frontmatter: [title]
optional_frontmatter: [tags]
---

# note

A bare-bones note skill. Replace this inline corpus with your tenant.
''',
    },
    instanceFiles: const {
      'note__welcome.md': '''---
type: instance
skill: note
id: welcome
title: Welcome
---

# Welcome

You are looking at the **escurel general editor** running against an
inline two-page fixture corpus. Open `note::welcome` to see the
frontmatter table, the body render, and any backlinks.

When the real backend is reachable (PR-6) the topbar mode chip
turns green and these placeholders disappear.
''',
    },
  );
}
