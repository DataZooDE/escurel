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

import '../app.dart';
import '../client/escurel_client.dart';
import '../client/fixture_escurel_client.dart';
import '../client/http_escurel_client.dart';
import '../client/models.dart';
import '../config/env.dart';

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
    AppMode.http when env.baseUrl.isNotEmpty => HttpEscurelClient(
        baseUrl: env.baseUrl,
        bearerToken: env.auth == AuthMode.bearer ? null : null,
      ),
    _ => _bootstrapInlineFixture(),
  };
  ref.onDispose(client.close);
  return client;
});

/// Currently focused page id. Null when the editor has nothing
/// opened (initial state).
final currentPageIdProvider = StateProvider<String?>((ref) => null);

/// Catalogue (skills + their instance counts).
final skillsCatalogueProvider = FutureProvider<List<SkillSummary>>((ref) {
  return ref.watch(escurelClientProvider).listSkills();
});

/// Instances of a given skill, keyed by skill id.
final instancesProvider = FutureProvider.family<List<InstanceSummary>, String>((ref, skillId) {
  return ref.watch(escurelClientProvider).listInstances(skillId);
});

/// The expanded current page, or null if nothing focused.
final currentPageProvider = FutureProvider<ExpandResult?>((ref) async {
  final id = ref.watch(currentPageIdProvider);
  if (id == null) return null;
  return ref.watch(escurelClientProvider).expand(id);
});

/// Backlinks (incoming neighbours) for the current page.
final currentBacklinksProvider = FutureProvider<List<Neighbour>>((ref) async {
  final id = ref.watch(currentPageIdProvider);
  if (id == null) return const <Neighbour>[];
  return ref.watch(escurelClientProvider).neighbours(id, direction: LinkDirection.incoming);
});

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
