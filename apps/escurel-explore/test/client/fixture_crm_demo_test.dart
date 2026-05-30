// No-mock test: the real FixtureEscurelClient over the real
// examples/crm-demo corpus answers the full surface the workspace uses —
// skills grouping, per-skill instances, neighbours (In/Out), events,
// inbox, snapshots — with real data, not a mock.

@TestOn('vm')
library;

import 'package:escurel_explore/client/models.dart';
import 'package:flutter_test/flutter_test.dart';

import '../support/crm_demo.dart';

void main() {
  final client = crmDemoClient();

  test('listSkills derives is_event_typed and spans both groups', () async {
    final skills = await client.listSkills();
    final byId = {for (final s in skills) s.id: s};
    expect(byId['customer']!.isEventTyped, isFalse);
    expect(byId['orgunit']!.isEventTyped, isFalse);
    expect(byId['meeting']!.isEventTyped, isTrue);
    expect(byId['email']!.isEventTyped, isTrue);
    expect(skills.where((s) => s.isEventTyped), isNotEmpty);
    expect(skills.where((s) => !s.isEventTyped), isNotEmpty);
  });

  test('listInstances is multi-account per skill', () async {
    Future<int> n(String s) async => (await client.listInstances(s)).length;
    expect(await n('customer'), greaterThanOrEqualTo(3));
    expect(await n('contact'), greaterThanOrEqualTo(6));
    expect(await n('workstream'), greaterThanOrEqualTo(4));
    expect(await n('orgunit'), greaterThanOrEqualTo(2));
  });

  test('the spine is a richly-connected hub (backlinks + outgoing)', () async {
    final backlinks =
        await client.neighbours(crmDemoSpineId, direction: LinkDirection.incoming);
    final outgoing =
        await client.neighbours(crmDemoSpineId, direction: LinkDirection.outgoing);
    expect(backlinks.length, greaterThanOrEqualTo(7));
    expect(outgoing.length, greaterThanOrEqualTo(5));
  });

  test('events, inbox and snapshots return real corpus data', () async {
    final history = await client.listEvents(crmDemoSpineId);
    expect(history, isNotEmpty);
    expect(history.every((e) => e.status == 'processed'), isTrue);

    final inbox = await client.listInbox();
    expect(inbox.length, greaterThanOrEqualTo(5));
    expect(inbox.every((e) => e.status == 'inbox'), isTrue);

    expect(await client.listSnapshots(crmDemoSpineId), hasLength(4));
    expect(await client.listSnapshots('engagement__ha-spine'), hasLength(3));
  });

  test('captureEvent appends to the inbox', () async {
    final before = (await client.listInbox()).length;
    final ev = await client.captureEvent(source: 'manual', title: 'probe', body: 'probe');
    expect(ev.status, 'inbox');
    expect((await client.listInbox()).length, before + 1);
  });
}
