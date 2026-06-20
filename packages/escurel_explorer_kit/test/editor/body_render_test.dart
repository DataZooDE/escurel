// No-mock widget tests for the entity editor's READ-side markdown body
// renderer. Pumps the real [EscurelExplorer] over a [FixtureEscurelClient]
// and asserts that inline markdown — bold, inline code, links, and the
// existing `[[wikilink]]` pills — renders as styled spans rather than raw
// source text. Mirrors the real KB corpus, which nests an inline code span
// inside a link (`[`community`](community.md)`).
//
// Assertions read the rendered `RichText` plain text (markers must be
// gone) and walk the inline spans (a span must actually carry the bold /
// monospace / link styling).

@TestOn('vm')
library;

import 'package:escurel_explorer_kit/escurel_explorer.dart';
import 'package:escurel_explorer_kit/widgets/wikilink_pill.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:escurel_explorer_kit/client/fixture_escurel_client.dart';

const _docSkill = '---\n'
    'type: skill\n'
    'id: doc\n'
    'description: A free-form document.\n'
    '---\n\n# doc\n';

// One instance whose body exercises every inline construct the renderer
// must handle, including a code span nested inside a link label.
const _richDoc = '---\n'
    'type: instance\n'
    'skill: doc\n'
    'id: rich\n'
    '---\n\n'
    '# Überschrift\n\n'
    'Ein **fetter** Begriff, ein `code_span` und ein '
    '[`community`](community.md)-Link sowie ein [[doc::rich]].\n\n'
    '- erster Punkt\n'
    '- zweiter Punkt\n';

FixtureEscurelClient _client() => FixtureEscurelClient.fromSources(
      skillFiles: {'doc.md': _docSkill},
      instanceFiles: {'doc__rich.md': _richDoc},
    );

Future<void> _open(WidgetTester tester) async {
  tester.view.physicalSize = const Size(1400, 1000);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(MaterialApp(home: EscurelExplorer(client: _client())));
  await tester.pumpAndSettle();
  await tester.tap(find.text('rich'));
  await tester.pumpAndSettle();
}

/// Concatenated visible text across every RichText under the body,
/// with WidgetSpan placeholders (`￼`) stripped.
String _bodyPlainText(WidgetTester tester) {
  final body = find.byKey(const ValueKey('entity_editor.body'));
  final texts = find.descendant(of: body, matching: find.byType(RichText));
  final buf = StringBuffer();
  for (final e in texts.evaluate()) {
    final rt = e.widget as RichText;
    buf.write(rt.text.toPlainText(includeSemanticsLabels: false).replaceAll('￼', ''));
  }
  return buf.toString();
}

/// Walk every inline span under the body and return the first whose
/// flattened text equals [needle], or null.
TextStyle? _styleOf(WidgetTester tester, String needle) {
  final body = find.byKey(const ValueKey('entity_editor.body'));
  for (final e in find.descendant(of: body, matching: find.byType(RichText)).evaluate()) {
    final rt = e.widget as RichText;
    TextStyle? hit;
    rt.text.visitChildren((span) {
      if (span is TextSpan && span.text == needle) {
        hit = span.style;
        return false;
      }
      return true;
    });
    if (hit != null) return hit;
  }
  return null;
}

void main() {
  testWidgets('bold renders as a bold span with no ** markers', (tester) async {
    await _open(tester);
    final plain = _bodyPlainText(tester);
    expect(plain, contains('fetter'));
    expect(plain, isNot(contains('**')));
    final style = _styleOf(tester, 'fetter');
    expect(style?.fontWeight, FontWeight.bold);
  });

  testWidgets('inline code renders monospace with no backtick markers', (tester) async {
    await _open(tester);
    final plain = _bodyPlainText(tester);
    expect(plain, contains('code_span'));
    expect(plain, isNot(contains('`')));
    final style = _styleOf(tester, 'code_span');
    expect(style?.fontFamily, isNotNull);
    expect(style?.fontFamily!.toLowerCase(), contains('mono'));
  });

  testWidgets('links render their label without []() source', (tester) async {
    await _open(tester);
    final plain = _bodyPlainText(tester);
    // The link label shows (the nested code span renders "community"),
    // but the markdown link source must be gone.
    expect(plain, contains('community'));
    expect(plain, isNot(contains('](')));
    expect(plain, isNot(contains('community.md')));
  });

  testWidgets('wikilink pills still render', (tester) async {
    await _open(tester);
    expect(find.byType(WikilinkPill), findsWidgets);
    final plain = _bodyPlainText(tester);
    expect(plain, isNot(contains('[[')));
  });

  testWidgets('bullet list items render with a bullet glyph', (tester) async {
    await _open(tester);
    final plain = _bodyPlainText(tester);
    expect(plain, contains('erster Punkt'));
    expect(plain, contains('zweiter Punkt'));
    // The raw "- " marker is replaced by a bullet glyph.
    expect(plain, contains('•'));
  });
}
