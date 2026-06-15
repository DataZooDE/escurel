// No-mock test for polling auto-refresh: a periodic timer invalidates the
// read providers so a watched view re-fetches without a manual reload.
// Disabling the toggle stops the polling.

@TestOn('vm')
library;

import 'package:escurel_explorer_kit/client/fixture_escurel_client.dart';
import 'package:escurel_explorer_kit/crm/auto_refresh.dart';
import 'package:escurel_explorer_kit/crm/crm_providers.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

const _skill =
    '---\ntype: skill\nid: talk\ndescription: A talk.\n---\n# talk\n';
const _inst = '---\ntype: instance\nskill: talk\nid: a\nname: A\n---\n# A\n';

FixtureEscurelClient _client() => FixtureEscurelClient.fromSources(
  skillFiles: {'talk.md': _skill},
  instanceFiles: {'talk__a.md': _inst},
);

void main() {
  testWidgets('polling re-resolves read providers; disabling stops it', (
    tester,
  ) async {
    var dataBuilds = 0;

    final container = ProviderContainer(
      overrides: [
        escurelClientProvider.overrideWithValue(_client()),
        autoRefreshIntervalProvider.overrideWith(
          (ref) => const Duration(milliseconds: 50),
        ),
      ],
    );
    addTearDown(container.dispose);

    await tester.pumpWidget(
      UncontrolledProviderScope(
        container: container,
        child: MaterialApp(
          home: AutoRefresher(
            child: Consumer(
              builder: (c, ref, _) {
                final v = ref.watch(allInstancesRawProvider);
                if (v.hasValue && !v.isLoading) dataBuilds++;
                return const SizedBox.shrink();
              },
            ),
          ),
        ),
      ),
    );
    // NB: never pumpAndSettle — the periodic timer keeps the tree busy and
    // would time it out. Drive the fake clock with explicit pumps.
    await tester.pump(); // first frame: postframe arms the timer, fetch starts
    await tester.pump(); // microtask: initial fetch resolves
    final afterInitial = dataBuilds;
    expect(
      afterInitial,
      greaterThanOrEqualTo(1),
      reason: 'initial fetch resolved',
    );

    // One poll interval → the timer fires → providers re-resolve.
    await tester.pump(const Duration(milliseconds: 60));
    await tester.pump(); // re-fetch resolves
    expect(
      dataBuilds,
      greaterThan(afterInitial),
      reason: 'polling re-fetched the data',
    );

    // Disable polling → no further re-resolves.
    container.read(autoRefreshEnabledProvider.notifier).state = false;
    await tester.pump();
    final afterDisable = dataBuilds;
    await tester.pump(const Duration(milliseconds: 120));
    await tester.pump();
    expect(
      dataBuilds,
      afterDisable,
      reason: 'disabled polling does not re-fetch',
    );
  });
}
