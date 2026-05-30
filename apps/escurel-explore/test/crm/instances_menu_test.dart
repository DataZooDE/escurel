// Widget test for the Instances crumb dropdown: opening it lists all
// instances grouped by skill; clicking one re-centres the workspace.

import 'package:escurel_explore/client/escurel_client.dart';
import 'package:escurel_explore/client/models.dart';
import 'package:escurel_explore/crm/crm_breadcrumb.dart';
import 'package:escurel_explore/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

const _acme = 'markdown/instances/customer__acme.md';
const _weber = 'markdown/instances/contact__weber.md';

SkillSummary _skill(String id) =>
    SkillSummary(id: id, description: id, requiredFrontmatter: const [], optionalFrontmatter: const []);

class _StubClient implements EscurelClient {
  @override
  Future<List<SkillSummary>> listSkills() async => [_skill('customer'), _skill('contact')];

  @override
  Future<List<InstanceSummary>> listInstances(String skillId,
      {Map<String, Object?>? filter, String? orderBy, int? limit, String? asOf, String? scenario}) async {
    if (skillId == 'customer') {
      return const [InstanceSummary(id: _acme, skill: 'customer', frontmatter: {'name': 'Acme Ltd'})];
    }
    if (skillId == 'contact') {
      return const [InstanceSummary(id: _weber, skill: 'contact', frontmatter: {'name': 'M. Weber'})];
    }
    return const [];
  }

  @override
  dynamic noSuchMethod(Invocation i) => throw UnimplementedError('${i.memberName}');
}

void main() {
  testWidgets('the Instances crumb lists instances grouped by skill and opens one', (tester) async {
    tester.view.physicalSize = const Size(1200, 800);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);

    final container = ProviderContainer(
      overrides: [escurelClientProvider.overrideWithValue(_StubClient())],
    );
    addTearDown(container.dispose);

    await tester.pumpWidget(
      UncontrolledProviderScope(
        container: container,
        child: const MaterialApp(home: Scaffold(appBar: CrmBreadcrumb(), body: SizedBox.shrink())),
      ),
    );
    await tester.pumpAndSettle();

    // The count crumb renders; the list is closed.
    expect(find.textContaining('Instances 2'), findsOneWidget);
    expect(find.bySemanticsLabel('instance-row:acme'), findsNothing);

    // Open it → grouped rows.
    await tester.tap(find.bySemanticsLabel('instances'));
    await tester.pumpAndSettle();
    expect(find.text('CONTACT · 1'), findsOneWidget);
    expect(find.text('CUSTOMER · 1'), findsOneWidget);
    expect(find.bySemanticsLabel('instance-row:acme'), findsOneWidget);
    expect(find.bySemanticsLabel('instance-row:weber'), findsOneWidget);
    expect(find.text('Acme Ltd'), findsOneWidget);

    // Click one → opens it, menu closes.
    await tester.tap(find.bySemanticsLabel('instance-row:acme'));
    await tester.pumpAndSettle();
    expect(container.read(currentPageIdProvider), _acme);
    expect(find.bySemanticsLabel('instance-row:acme'), findsNothing);
  });
}
