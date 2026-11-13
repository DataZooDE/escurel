// A skill that declares `schedule:` is fired by the runner on that cadence
// (references/11), so wherever skills are listed the cron expression — and
// its `schedule_budget.runs_per_day` cap, when set — is shown: a
// ScheduleBadge (`skill-schedule:<id>`) in the catalogue header and a line
// in the ☰ skills registry. Unscheduled skills show nothing.

import 'package:escurel_explorer_kit/client/fixture_escurel_client.dart';
import 'package:escurel_explorer_kit/client/models.dart';
import 'package:escurel_explorer_kit/crm/skills_menu.dart';
import 'package:escurel_explorer_kit/editor/catalogue_pane.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

/// The skills come from real skill pages through the fixture client, so
/// the frontmatter → SkillSummary parse is exercised too.
FixtureEscurelClient _client() => FixtureEscurelClient.fromSources(
  skillFiles: const {
    'digest.md':
        '---\nkind: skill\nid: digest\ndescription: Weekday digest.\nschedule: "0 9 * * 1-5"\nschedule_budget:\n  runs_per_day: 3\n---\n\n# digest',
    'sweep.md':
        '---\nkind: skill\nid: sweep\ndescription: Hourly sweep.\nschedule: "@hourly"\n---\n\n# sweep',
    'note.md':
        '---\nkind: skill\nid: note\ndescription: A note.\n---\n\n# note',
  },
  instanceFiles: const {},
);

Future<void> _pump(WidgetTester tester, Widget body) async {
  tester.view.physicalSize = const Size(1400, 1000);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(
    ProviderScope(
      overrides: [
        escurelClientProvider.overrideWithValue(_client()),
        instancesProvider.overrideWith(_EmptyInstances.new),
      ],
      child: MaterialApp(home: Scaffold(body: body)),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('the catalogue shows a scheduled skill\'s cron + budget', (
    tester,
  ) async {
    await _pump(tester, const CataloguePane());

    expect(find.bySemanticsLabel('skill-schedule:digest'), findsOneWidget);
    expect(find.text('0 9 * * 1-5 · ≤3/day'), findsOneWidget);
    expect(find.bySemanticsLabel('skill-schedule:sweep'), findsOneWidget);
    expect(find.text('@hourly'), findsOneWidget);
    expect(find.bySemanticsLabel('skill-schedule:note'), findsNothing);
  });

  testWidgets('the ☰ skills registry shows the schedule too', (tester) async {
    await _pump(tester, const Align(child: SkillsMenu()));
    await tester.tap(find.bySemanticsLabel('skills-menu'));
    await tester.pumpAndSettle();

    expect(find.bySemanticsLabel('skill-row:digest'), findsOneWidget);
    expect(find.text('0 9 * * 1-5 · ≤3/day'), findsOneWidget);
    expect(find.text('@hourly'), findsOneWidget);
  });
}

class _EmptyInstances extends InstancesOfSkill {
  @override
  Future<InstancePage> build(String skillId) async =>
      const InstancePage(instances: []);
}
