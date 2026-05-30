// Widget test for the instance↔skill dual affordance: default tap →
// primary; shift-click → skill; long hover reveals a clickable "→ skill"
// chip → skill; a brief hover reveals nothing.

import 'package:escurel_explore/widgets/instance_skill_link.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

const _hostKey = Key('host');

Future<({int Function() primary, int Function() skill})> _pump(WidgetTester tester) async {
  var primary = 0;
  var skill = 0;
  tester.view.physicalSize = const Size(800, 600);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(
    MaterialApp(
      home: Scaffold(
        body: Center(
          child: InstanceSkillLink(
            skillLabel: 'customer',
            onPrimary: () => primary++,
            onSkill: () => skill++,
            child: const SizedBox(
              key: _hostKey,
              width: 160,
              height: 28,
              child: Center(child: Text('customer::muenchner-pharma')),
            ),
          ),
        ),
      ),
    ),
  );
  await tester.pumpAndSettle();
  return (primary: () => primary, skill: () => skill);
}

void main() {
  testWidgets('plain tap fires the primary (instance) action', (tester) async {
    final c = await _pump(tester);
    await tester.tap(find.byKey(_hostKey));
    await tester.pumpAndSettle();
    expect(c.primary(), 1);
    expect(c.skill(), 0);
    expect(find.bySemanticsLabel('goto-skill:customer'), findsNothing);
  });

  testWidgets('shift-click fires the skill action', (tester) async {
    final c = await _pump(tester);
    await tester.sendKeyDownEvent(LogicalKeyboardKey.shiftLeft);
    await tester.tap(find.byKey(_hostKey));
    await tester.sendKeyUpEvent(LogicalKeyboardKey.shiftLeft);
    await tester.pumpAndSettle();
    expect(c.skill(), 1);
    expect(c.primary(), 0);
  });

  testWidgets('long hover reveals a → skill chip; clicking it fires the skill action',
      (tester) async {
    final c = await _pump(tester);
    final gesture = await tester.createGesture(kind: PointerDeviceKind.mouse);
    await gesture.addPointer(location: Offset.zero);
    addTearDown(() => gesture.removePointer());
    await tester.pump();

    // A brief hover shows nothing.
    await gesture.moveTo(tester.getCenter(find.byKey(_hostKey)));
    await tester.pump(const Duration(milliseconds: 120));
    expect(find.bySemanticsLabel('goto-skill:customer'), findsNothing);

    // After the dwell the chip appears.
    await tester.pump(const Duration(milliseconds: 700));
    expect(find.bySemanticsLabel('goto-skill:customer'), findsOneWidget);

    // Clicking it navigates to the skill (and not the instance).
    await tester.tap(find.bySemanticsLabel('goto-skill:customer'));
    await tester.pumpAndSettle();
    expect(c.skill(), 1);
    expect(c.primary(), 0);
  });
}
