// No-mock widget test for the links footer: the real FixtureEscurelClient
// over the real crm-demo corpus. The spine is a richly-connected hub, so
// BACKLINKS show the source pages and OUTGOING LINKS the targets; a chip
// re-centres the workspace on that neighbour.

@TestOn('vm')
library;

import 'package:escurel_explorer_kit/crm/links_footer.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

import '../support/crm_demo.dart';

void main() {
  testWidgets('renders real backlinks + outgoing links; a chip re-centres focus', (tester) async {
    final container = ProviderContainer(
      overrides: [
        escurelClientProvider.overrideWithValue(crmDemoClient()),
        currentPageIdProvider.overrideWith((ref) => crmDemoSpineId),
      ],
    );
    addTearDown(container.dispose);

    await tester.pumpWidget(
      UncontrolledProviderScope(
        container: container,
        child: const MaterialApp(
          home: Scaffold(body: SingleChildScrollView(child: LinksFooter())),
        ),
      ),
    );
    await tester.pumpAndSettle();

    expect(find.bySemanticsLabel('links-footer'), findsOneWidget);
    expect(find.textContaining('BACKLINKS ·'), findsOneWidget);
    expect(find.textContaining('OUTGOING LINKS ·'), findsOneWidget);

    // A backlink shows its *source* page (reiter links to the spine); an
    // outgoing link shows its *target* (the spine links to the lead).
    expect(find.bySemanticsLabel('backlink:reiter'), findsWidgets);
    expect(find.bySemanticsLabel('outlink:hoffmann-followup'), findsWidgets);

    // Tapping a backlink re-centres on the source instance.
    await tester.tap(find.bySemanticsLabel('backlink:reiter').first);
    await tester.pumpAndSettle();
    expect(container.read(currentPageIdProvider), 'contact__reiter');
    expect(container.read(navBackStackProvider), contains(crmDemoSpineId));
  });
}
