import 'package:escurel_explore/app.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  testWidgets('shell renders topbar brand, three pane placeholders, status bar', (tester) async {
    await tester.pumpWidget(const ProviderScope(child: EscurelExploreApp()));
    await tester.pumpAndSettle();

    expect(find.text('escurel-explore'), findsOneWidget);

    expect(find.byKey(const ValueKey('pane.catalogue')), findsOneWidget);
    expect(find.byKey(const ValueKey('pane.editor')), findsOneWidget);
    expect(find.byKey(const ValueKey('pane.right')), findsOneWidget);

    expect(find.byKey(const ValueKey('shell.status_bar')), findsOneWidget);
  });

  testWidgets('topbar shows mode chip reflecting fixture default', (tester) async {
    await tester.pumpWidget(const ProviderScope(child: EscurelExploreApp()));
    await tester.pumpAndSettle();

    expect(find.text('fixture'), findsOneWidget);
  });
}
