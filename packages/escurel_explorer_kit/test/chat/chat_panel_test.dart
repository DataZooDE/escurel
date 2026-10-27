// Widget test for the Chat tab (governed chat agent, BRD FR-E1–E3), over the
// in-memory fixture client seeded with the shipped chat corpus
// (examples/chat/seed). The fixture has no runner, so the test plays the
// runner's part: it appends the assistant turn the poll must pick up.

import 'dart:io';

import 'package:escurel_explorer_kit/chat/chat_panel.dart';
import 'package:escurel_explorer_kit/chat/chat_state.dart';
import 'package:escurel_explorer_kit/client/escurel_client.dart';
import 'package:escurel_explorer_kit/client/fixture_escurel_client.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

String _seed(String name) =>
    File('../../examples/chat/seed/skills/$name').readAsStringSync();

EscurelClient _corpus() => FixtureEscurelClient.fromSources(
  skillFiles: {
    'conversation.md': _seed('conversation.md'),
    'chat.md': _seed('chat.md'),
  },
  instanceFiles: const {},
  writeEnabled: true,
);

Future<void> _pump(WidgetTester tester, EscurelClient client) async {
  tester.view.physicalSize = const Size(500, 900);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(
    ProviderScope(
      overrides: [escurelClientProvider.overrideWithValue(client)],
      child: const MaterialApp(home: Scaffold(body: ChatPanel())),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  test('the chat group id is the gateway\'s page-chat id', () {
    expect(
      chatGroupId('alice', 'markdown/instances/conversation/c-1.md'),
      'page-chat:v1:2bd806c97f0e00af1a1fc3328fa763a9269723c8db8fac4f93af71db186d6e90:'
      'markdown/instances/conversation/c-1.md',
    );
  });

  testWidgets('a question shows at once, then the answer the poll picks up', (
    tester,
  ) async {
    final client = _corpus();
    await _pump(tester, client);
    expect(find.bySemanticsLabel('chat-panel'), findsOneWidget);
    expect(find.bySemanticsLabel('chat-input'), findsOneWidget);
    expect(find.bySemanticsLabel('chat-send'), findsOneWidget);

    await tester.enterText(find.byType(TextField), 'Which customers renew?');
    await tester.tap(find.text('Send'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 100));

    // The conversation was created and named after the question.
    await tester.tap(find.text('Recent'));
    await tester.pump(const Duration(milliseconds: 300));
    final recent = find.bySemanticsLabel(
      RegExp(r'^chat-recent:markdown/instances/conversation/'),
    );
    expect(recent, findsOneWidget);
    expect(find.bySemanticsLabel(RegExp(r'^chat-message:0\b')), findsOneWidget);
    expect(find.text('Working on it…'), findsOneWidget);

    // Play the runner: answer the turn into the same group.
    final container = ProviderScope.containerOf(
      tester.element(find.byType(ChatPanel)),
    );
    final pageId = container.read(chatControllerProvider).current!.pageId;
    final group = chatGroupId('fixture-user', pageId);
    final userTurn = (await client.listMessages(group)).messages.single;
    await client.appendMessage(
      chatGroupId: group,
      role: 'assistant',
      content: 'Globex renews. See [[customer::globex]].',
      msgId: 'run:R1:reply',
      metadata: {'status': 'ok', 'user_msg_id': userTurn.msgId},
    );
    await tester.pump(const Duration(seconds: 2));
    await tester.pump();

    expect(find.text('Working on it…'), findsNothing);
    expect(find.bySemanticsLabel(RegExp(r'^chat-message:1\b')), findsOneWidget);
    expect(find.textContaining('Globex renews.'), findsOneWidget);
    expect(find.widgetWithText(ActionChip, 'customer::globex'), findsOneWidget);
  });

  testWidgets('says so when the chat skills are not installed', (tester) async {
    await _pump(
      tester,
      FixtureEscurelClient.fromSources(
        skillFiles: const {},
        instanceFiles: const {},
      ),
    );
    expect(find.textContaining('Chat is not set up here'), findsOneWidget);
    expect(find.bySemanticsLabel('chat-input'), findsNothing);
  });
}
