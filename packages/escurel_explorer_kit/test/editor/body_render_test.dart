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

  // A code span whose content is a known skill id becomes a clickable
  // link to that skill page; a code span that is just a field name stays
  // inert. (The venue skill's `` `event` `` should jump to the event skill;
  // `` `name` `` / `` `address` `` should not.)
  testWidgets('code span matching a skill id links to that skill; a field name does not',
      (tester) async {
    final client = FixtureEscurelClient.fromSources(
      skillFiles: {
        'doc.md': _docSkill,
        'event.md': '---\ntype: skill\nid: event\n'
            'description: Ein Event.\n---\n\n# event\n\nEVENTBODYMARKER\n',
      },
      instanceFiles: {
        'doc__venueish.md': '---\ntype: instance\nskill: doc\nid: venueish\n---\n\n'
            '# venueish\n\n'
            'Das Feld `event:` verweist auf das `event`, optional `name`.\n',
      },
    );
    tester.view.physicalSize = const Size(1400, 1000);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    await tester.pumpWidget(MaterialApp(home: EscurelExplorer(client: client)));
    await tester.pumpAndSettle();
    await tester.tap(find.text('venueish'));
    await tester.pumpAndSettle();

    // `event` (a skill) renders as a wikilink pill — the same affordance
    // the frontmatter uses; `name` (a field) and `event:` (trailing colon,
    // ≠ the skill id) stay plain code, not pills.
    expect(find.widgetWithText(WikilinkPill, 'event'), findsOneWidget);
    expect(find.widgetWithText(WikilinkPill, 'name'), findsNothing);

    // Not yet on the event page.
    expect(find.textContaining('EVENTBODYMARKER', findRichText: true), findsNothing);

    await tester.tap(find.widgetWithText(WikilinkPill, 'event'));
    await tester.pumpAndSettle();

    // Navigated to the event skill page.
    expect(find.textContaining('EVENTBODYMARKER', findRichText: true), findsWidgets);
  });

  // The frontmatter table linkifies the same way: a value (or list item)
  // that is a known skill id renders as a pill — e.g. a skill's
  // `required_frontmatter: [event]` lets you jump to the event skill.
  testWidgets('frontmatter values that name a skill render as pills', (tester) async {
    final client = FixtureEscurelClient.fromSources(
      skillFiles: {
        'doc.md': _docSkill,
        'event.md': '---\ntype: skill\nid: event\ndescription: Ein Event.\n---\n\n# event\n',
      },
      instanceFiles: {
        // `ref` is a SCALAR skill id (reproduces the event skill's
        // `id: event` row); `label` is a plain word.
        'doc__fm.md': '---\ntype: instance\nskill: doc\nid: fm\n'
            'ref: event\nlabel: name\n---\n\n# fm\n',
      },
    );
    tester.view.physicalSize = const Size(1400, 1000);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    await tester.pumpWidget(MaterialApp(home: EscurelExplorer(client: client)));
    await tester.pumpAndSettle();
    await tester.tap(find.text('fm'));
    await tester.pumpAndSettle();

    final fm = find.byKey(const ValueKey('entity_editor.frontmatter'));
    // `event` (a skill) → pill; `name` (just a word) → plain text.
    final eventPill = find.descendant(of: fm, matching: find.widgetWithText(WikilinkPill, 'event'));
    expect(eventPill, findsOneWidget);
    expect(find.descendant(of: fm, matching: find.widgetWithText(WikilinkPill, 'name')),
        findsNothing);
    // The pill sizes to its content — it must not stretch across the row's
    // Expanded slot (regression: a bare pill in Expanded filled the width).
    expect(tester.getSize(eventPill).width, lessThan(200));
  });
}
