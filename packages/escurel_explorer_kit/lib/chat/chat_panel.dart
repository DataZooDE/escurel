/// The Chat tab of the right rail (governed chat agent, BRD FR-E1–E3):
/// Recent conversations, New chat, the conversation and a prompt. The
/// Semantics labels are the verify-demo selector contract — `chat-panel`,
/// `chat-input`, `chat-send`, `chat-message:<i>`, `chat-recent:<page_id>` —
/// don't rename them casually.
library;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../state/providers.dart';
import '../theme/app_theme.dart';
import 'chat_state.dart';

final _wikilink = RegExp(r'\[\[([^\]|]+)(?:\|[^\]]*)?\]\]');

class ChatPanel extends ConsumerStatefulWidget {
  const ChatPanel({super.key});

  @override
  ConsumerState<ChatPanel> createState() => _ChatPanelState();
}

class _ChatPanelState extends ConsumerState<ChatPanel> {
  final _input = TextEditingController();

  @override
  void dispose() {
    _input.dispose();
    super.dispose();
  }

  void _send() {
    final text = _input.text;
    if (text.trim().isEmpty) return;
    _input.clear();
    ref.read(chatControllerProvider.notifier).send(text);
  }

  /// A cited `[[skill::id]]` opens the record (`/p/:pageId`).
  Future<void> _openLink(String target) async {
    final resolved = await ref
        .read(escurelClientProvider)
        .resolve('[[$target]]');
    if (resolved.exists && resolved.pageId.isNotEmpty) {
      navigateToInstance(ref, resolved.pageId);
    }
  }

  @override
  Widget build(BuildContext context) {
    final s = ref.watch(chatControllerProvider);
    final c = ref.read(chatControllerProvider.notifier);
    return Semantics(
      label: 'chat-panel',
      container: true,
      explicitChildNodes: true,
      // A Material, not a coloured box: the Recent tiles paint their ink on it.
      child: Material(
        color: kSurfaceContainerLow,
        child: !s.ready
            ? Padding(
                padding: const EdgeInsets.all(12),
                child: Text(
                  s.message ?? 'Loading…',
                  style: const TextStyle(color: kOnSurfaceVariant),
                ),
              )
            : Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  Padding(
                    padding: const EdgeInsets.fromLTRB(12, 8, 8, 4),
                    child: Row(
                      children: [
                        Expanded(
                          child: Text(
                            s.current?.title ?? 'New chat',
                            overflow: TextOverflow.ellipsis,
                            style: const TextStyle(fontWeight: FontWeight.w600),
                          ),
                        ),
                        Semantics(
                          label: 'chat-new',
                          button: true,
                          child: TextButton(
                            onPressed: c.newChat,
                            child: const Text('New chat'),
                          ),
                        ),
                      ],
                    ),
                  ),
                  if (s.recent.isNotEmpty)
                    ExpansionTile(
                      title: const Text('Recent'),
                      dense: true,
                      children: [
                        for (final r in s.recent)
                          Semantics(
                            label: 'chat-recent:${r.pageId}',
                            button: true,
                            child: ListTile(
                              dense: true,
                              title: Text(
                                r.title,
                                overflow: TextOverflow.ellipsis,
                              ),
                              selected: s.current?.pageId == r.pageId,
                              onTap: () => c.open(r),
                            ),
                          ),
                      ],
                    ),
                  const Divider(height: 1),
                  Expanded(
                    child: s.rows.isEmpty
                        ? const Padding(
                            padding: EdgeInsets.all(12),
                            child: Text(
                              'Ask a question about what this knowledge base knows.',
                              style: TextStyle(color: kOnSurfaceVariant),
                            ),
                          )
                        : ListView.builder(
                            padding: const EdgeInsets.all(8),
                            itemCount: s.rows.length,
                            itemBuilder: (_, i) => _row(s.rows[i], i),
                          ),
                  ),
                  if (s.message != null)
                    Padding(
                      padding: const EdgeInsets.symmetric(horizontal: 12),
                      child: Text(
                        s.message!,
                        style: const TextStyle(color: kError),
                      ),
                    ),
                  Padding(
                    padding: const EdgeInsets.all(8),
                    child: Row(
                      crossAxisAlignment: CrossAxisAlignment.end,
                      children: [
                        Expanded(
                          child: Semantics(
                            label: 'chat-input',
                            textField: true,
                            child: CallbackShortcuts(
                              // Enter sends; Shift+Enter is a new line.
                              bindings: {
                                const SingleActivator(LogicalKeyboardKey.enter):
                                    _send,
                              },
                              child: TextField(
                                controller: _input,
                                minLines: 1,
                                maxLines: 5,
                                decoration: const InputDecoration(
                                  hintText: 'Ask a question',
                                  isDense: true,
                                  border: OutlineInputBorder(),
                                ),
                              ),
                            ),
                          ),
                        ),
                        const SizedBox(width: 6),
                        Semantics(
                          label: 'chat-send',
                          button: true,
                          child: FilledButton(
                            onPressed: s.sending ? null : _send,
                            child: const Text('Send'),
                          ),
                        ),
                      ],
                    ),
                  ),
                ],
              ),
      ),
    );
  }

  Widget _row(ChatRow r, int i) {
    if (r.kind == 'pending') {
      return Semantics(
        label: 'chat-message:$i',
        liveRegion: true,
        child: const Padding(
          padding: EdgeInsets.symmetric(vertical: 6),
          child: Text(
            'Working on it…',
            style: TextStyle(
              fontStyle: FontStyle.italic,
              color: kOnSurfaceVariant,
            ),
          ),
        ),
      );
    }
    final mine = r.kind == 'user';
    final failed = r.status == 'failed' || r.status == 'cancelled';
    final who = mine
        ? 'You'
        : (failed
              ? 'Escurel · ${r.status == 'failed' ? 'Failed' : 'Stopped'}'
              : 'Escurel');
    final links = mine
        ? const <String>[]
        : _wikilink
              .allMatches(r.text)
              .map((m) => m.group(1)!.trim())
              .toSet()
              .toList();
    return Semantics(
      label: 'chat-message:$i',
      container: true,
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 6),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(
              who,
              style: const TextStyle(fontSize: 12, color: kOnSurfaceVariant),
            ),
            Container(
              padding: const EdgeInsets.only(left: 8),
              decoration: BoxDecoration(
                border: Border(
                  left: BorderSide(
                    width: 2,
                    color: mine ? kPrimary : (failed ? kError : kSecondary),
                  ),
                ),
              ),
              child: SelectableText(r.text),
            ),
            if (links.isNotEmpty)
              Wrap(
                spacing: 4,
                children: [
                  for (final l in links)
                    ActionChip(label: Text(l), onPressed: () => _openLink(l)),
                ],
              ),
          ],
        ),
      ),
    );
  }
}
