// No-mock test for hiding erased (tombstoned) instances. Erasure is
// signalled by `status: erased` (members) / `status: revoked` (consent) in
// frontmatter — exactly what Carl's `erase_member` writes. By default the
// explorer hides them; a toggle reveals them (struck-through). Real
// FixtureEscurelClient over a tiny in-memory corpus.

@TestOn('vm')
library;

import 'package:escurel_explorer_kit/client/fixture_escurel_client.dart';
import 'package:escurel_explorer_kit/client/models.dart';
import 'package:escurel_explorer_kit/crm/crm_breadcrumb.dart';
import 'package:escurel_explorer_kit/crm/crm_providers.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

const _memberSkill =
    '---\ntype: skill\nid: community_member\n'
    'description: A member.\n---\n# community_member\n';
const _alice =
    '---\ntype: instance\nskill: community_member\nid: alice\n'
    'name: Alice\ncredential: "whatsapp:111"\n---\n# Alice\n';
// A tombstoned member, exactly as erase_member leaves it.
const _bob =
    '---\ntype: instance\nskill: community_member\nid: bob\n'
    'name: Bob\ncredential: "whatsapp:222"\nstatus: erased\n'
    'erased_at: "2026-06-15T00:00:00Z"\n---\n# Bob\nGelöscht auf Nutzerwunsch.\n';

FixtureEscurelClient _client() => FixtureEscurelClient.fromSources(
  skillFiles: {'community_member.md': _memberSkill},
  instanceFiles: {
    'community_member__alice.md': _alice,
    'community_member__bob.md': _bob,
  },
);

void main() {
  test('InstanceSummary.erased keys on status erased/revoked', () {
    const live = InstanceSummary(
      id: 'community_member__alice',
      skill: 'community_member',
      frontmatter: {'id': 'alice'},
    );
    const erased = InstanceSummary(
      id: 'community_member__bob',
      skill: 'community_member',
      frontmatter: {'id': 'bob', 'status': 'erased'},
    );
    const revoked = InstanceSummary(
      id: 'platform_consent__bob',
      skill: 'platform_consent',
      frontmatter: {'status': 'revoked'},
    );
    expect(live.erased, isFalse);
    expect(erased.erased, isTrue);
    expect(revoked.erased, isTrue);
  });

  test(
    'allInstancesProvider hides erased by default, reveals when toggled',
    () async {
      final container = ProviderContainer(
        overrides: [escurelClientProvider.overrideWithValue(_client())],
      );
      addTearDown(container.dispose);

      final hidden = await container.read(allInstancesProvider.future);
      expect(hidden.map((i) => i.id), [
        'community_member__alice',
      ], reason: 'erased bob is filtered out by default');

      container.read(showErasedProvider.notifier).state = true;
      final shown = await container.read(allInstancesProvider.future);
      expect(
        shown.map((i) => i.id).toSet(),
        {'community_member__alice', 'community_member__bob'},
        reason: 'toggling showErased reveals the tombstone',
      );
    },
  );

  testWidgets(
    'Instances menu hides erased rows by default, toggle reveals them',
    (tester) async {
      tester.view.physicalSize = const Size(1200, 1000);
      tester.view.devicePixelRatio = 1.0;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);

      final container = ProviderContainer(
        overrides: [escurelClientProvider.overrideWithValue(_client())],
      );
      addTearDown(container.dispose);

      await tester.pumpWidget(
        UncontrolledProviderScope(
          container: container,
          child: const MaterialApp(
            home: Scaffold(appBar: CrmBreadcrumb(), body: SizedBox.shrink()),
          ),
        ),
      );
      await tester.pumpAndSettle();

      // Count reflects only the live instance.
      expect(find.text('Instances 1'), findsOneWidget);

      await tester.tap(find.bySemanticsLabel('instances'));
      await tester.pumpAndSettle();
      expect(find.bySemanticsLabel('instance-row:alice'), findsOneWidget);
      expect(find.bySemanticsLabel('instance-row:bob'), findsNothing);

      // Reveal erased.
      await tester.tap(find.bySemanticsLabel('show-erased-toggle'));
      await tester.pumpAndSettle();
      expect(
        find.bySemanticsLabel('instance-row:bob'),
        findsOneWidget,
        reason: 'the tombstone appears when revealed',
      );
    },
  );
}
