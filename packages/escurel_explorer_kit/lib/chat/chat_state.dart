/// The Chat tab's state (governed chat agent, BRD FR-E1/E2): a person
/// asks about the knowledge base and every answer is an `escurel-runner`
/// run. A conversation is an instance of the chat corpus's
/// `conversation` skill with a page-bound chat group; a question is
/// `append_message` (the person's turn) plus a `chat` event the runner
/// answers. Explore has no WebSocket client, so a pending question is
/// answered by polling `list_messages`.
library;

import 'dart:async';
import 'dart:convert';
import 'dart:math';

import 'package:crypto/crypto.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../client/escurel_client.dart';
import '../client/models.dart';
import '../state/providers.dart';

/// `page-chat:v1:<sha256(subject)>:<page_id>`, exactly as the gateway
/// names a page-bound chat group.
String chatGroupId(String subject, String pageId) =>
    'page-chat:v1:${sha256.convert(utf8.encode(subject))}:$pageId';

/// One conversation in Recent.
class Conversation {
  const Conversation({required this.pageId, required this.title});
  final String pageId;
  final String title;
}

/// One row of the conversation.
class ChatRow {
  const ChatRow({
    required this.kind,
    required this.text,
    this.status = 'ok',
    this.msgId = '',
  });

  /// `user`, `assistant` or `pending`.
  final String kind;
  final String text;

  /// An answer's `metadata.status`: `ok`, `failed` or `cancelled`.
  final String status;
  final String msgId;
}

/// Messages (any order, maybe twice) → rows, oldest first, with a working
/// line when the newest question has no answer yet.
List<ChatRow> foldChat(Iterable<ChatMessage> messages) {
  final byId = <String, ChatMessage>{for (final m in messages) m.msgId: m};
  final sorted = byId.values.toList()
    ..sort(
      (a, b) =>
          a.ts == b.ts ? a.msgId.compareTo(b.msgId) : a.ts.compareTo(b.ts),
    );
  final answered = {
    for (final m in sorted)
      if (m.role == 'assistant' && m.metadata?['user_msg_id'] is String)
        m.metadata!['user_msg_id'] as String,
  };
  final rows = <ChatRow>[
    for (final m in sorted)
      if (m.role == 'user' || m.role == 'assistant')
        ChatRow(
          kind: m.role,
          text: m.content,
          msgId: m.msgId,
          status: (m.metadata?['status'] as String?) ?? 'ok',
        ),
  ];
  final lastUser = sorted.lastWhere(
    (m) => m.role == 'user',
    orElse: () => const ChatMessage(
      chatGroupId: '',
      msgId: '',
      ts: '',
      role: '',
      content: '',
      embedded: false,
    ),
  );
  if (lastUser.msgId.isNotEmpty && !answered.contains(lastUser.msgId)) {
    rows.add(const ChatRow(kind: 'pending', text: 'Working on it…'));
  }
  return rows;
}

class ChatState {
  const ChatState({
    this.ready = false,
    this.message,
    this.recent = const [],
    this.current,
    this.rows = const [],
    this.sending = false,
  });

  final bool ready;

  /// One line when chat cannot be offered (not set up, or an error).
  final String? message;
  final List<Conversation> recent;
  final Conversation? current;
  final List<ChatRow> rows;
  final bool sending;

  bool get pending => rows.isNotEmpty && rows.last.kind == 'pending';

  ChatState copyWith({
    bool? ready,
    String? message,
    List<Conversation>? recent,
    Conversation? current,
    bool clearCurrent = false,
    List<ChatRow>? rows,
    bool? sending,
  }) => ChatState(
    ready: ready ?? this.ready,
    message: message ?? this.message,
    recent: recent ?? this.recent,
    current: clearCurrent ? null : (current ?? this.current),
    rows: rows ?? this.rows,
    sending: sending ?? this.sending,
  );
}

class ChatController extends StateNotifier<ChatState> {
  ChatController(
    this._client, {
    this.pollEvery = const Duration(milliseconds: 1500),
  }) : super(const ChatState());

  final EscurelClient _client;
  final Duration pollEvery;

  /// How long a question is polled for before the tab stops waiting.
  static const pollFor = Duration(seconds: 150);

  String? _subject;
  final _messages = <String, ChatMessage>{};
  Timer? _poll;
  DateTime? _pollUntil;

  Future<void> load() async {
    try {
      _subject = (await _client.whoami()).subject;
      final skills = (await _client.listSkills()).map((s) => s.id).toSet();
      if (!skills.contains('chat') || !skills.contains('conversation')) {
        state = state.copyWith(
          message:
              'Chat is not set up here. An admin can install the chat skills '
              '(examples/chat/seed).',
        );
        return;
      }
      final page = await _client.listInstances(
        'conversation',
        orderBy: 'at desc',
        limit: 30,
      );
      state = state.copyWith(
        ready: true,
        recent: [
          for (final i in page.instances)
            Conversation(
              pageId: i.id,
              title: (i.frontmatter['title'] as String?) ?? i.id,
            ),
        ],
      );
    } catch (e) {
      state = state.copyWith(message: 'Could not load chat: $e');
    }
  }

  void newChat() {
    _stopPolling();
    _messages.clear();
    state = state.copyWith(clearCurrent: true, rows: const []);
  }

  Future<void> open(Conversation c) async {
    _stopPolling();
    _messages.clear();
    state = state.copyWith(current: c, rows: const []);
    await _refresh();
  }

  Future<void> send(String raw) async {
    final text = raw.trim();
    final subject = _subject;
    if (text.isEmpty || subject == null || state.sending || !state.ready) {
      return;
    }
    state = state.copyWith(sending: true);
    try {
      var current = state.current;
      if (current == null) {
        current = await _createConversation(subject, text);
        state = state.copyWith(
          current: current,
          recent: [current, ...state.recent],
        );
      }
      final group = chatGroupId(subject, current.pageId);
      final msgId = _uuid();
      await _client.appendMessage(
        chatGroupId: group,
        role: 'user',
        content: text,
        msgId: msgId,
      );
      await _client.captureEvent(
        labelSkill: 'chat',
        source: 'explore',
        instancePageId: current.pageId,
        title: 'chat turn',
        provenance: {'chat_group_id': group, 'user_msg_id': msgId},
      );
      await _refresh();
      _startPolling();
    } catch (e) {
      state = state.copyWith(message: 'Could not send your question: $e');
    } finally {
      state = state.copyWith(sending: false);
    }
  }

  Future<Conversation> _createConversation(
    String owner,
    String question,
  ) async {
    final now = DateTime.now().toUtc();
    final stamp = now
        .toIso8601String()
        .replaceAll(RegExp(r'[-:T.Z]'), '')
        .substring(0, 14);
    final id = 'c-$stamp-${Random().nextInt(0xffffff).toRadixString(16)}';
    final pageId = 'markdown/instances/conversation/$id.md';
    final title = question.length > 60
        ? '${question.substring(0, 59)}…'
        : question;
    final startedAt = '${now.toIso8601String().split('.').first}Z';
    await _client.updatePage(
      pageId,
      '---\nkind: instance\nskill: conversation\nid: $id\ntitle: ${jsonEncode(title)}\n'
      'owner: ${jsonEncode(owner)}\nstarted_at: $startedAt\n---\n# ${title.replaceAll('\n', ' ')}\n',
    );
    return Conversation(pageId: pageId, title: title);
  }

  Future<void> _refresh() async {
    final current = state.current;
    final subject = _subject;
    if (current == null || subject == null) return;
    final page = await _client.listMessages(
      chatGroupId(subject, current.pageId),
      limit: 200,
    );
    for (final m in page.messages) {
      _messages[m.msgId] = m;
    }
    if (state.current?.pageId != current.pageId) return;
    state = state.copyWith(rows: foldChat(_messages.values));
    if (!state.pending) {
      _stopPolling();
    }
  }

  void _startPolling() {
    _pollUntil = DateTime.now().add(pollFor);
    _poll ??= Timer.periodic(pollEvery, (_) async {
      if (_pollUntil != null && DateTime.now().isAfter(_pollUntil!)) {
        _stopPolling();
        return;
      }
      await _refresh();
    });
  }

  void _stopPolling() {
    _poll?.cancel();
    _poll = null;
  }

  static String _uuid() {
    final r = Random.secure();
    final b = List<int>.generate(16, (_) => r.nextInt(256));
    final h = b.map((x) => x.toRadixString(16).padLeft(2, '0')).join();
    return '${h.substring(0, 8)}-${h.substring(8, 12)}-${h.substring(12, 16)}-'
        '${h.substring(16, 20)}-${h.substring(20)}';
  }

  @override
  void dispose() {
    _stopPolling();
    super.dispose();
  }
}

final chatControllerProvider =
    StateNotifierProvider.autoDispose<ChatController, ChatState>(
      (ref) => ChatController(ref.watch(escurelClientProvider))..load(),
    );
