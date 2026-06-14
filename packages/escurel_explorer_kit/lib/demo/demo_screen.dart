/// Capabilities demo surface — one tab per backend surface, wired to
/// the live [EscurelClient]. This is the screen the rodney
/// browser-verification harness drives (`scripts/verify-demo.sh`).
///
/// **rodney contract:** Flutter web renders to a CanvasKit `<canvas>`
/// with no CSS-selectable DOM, so rodney drives this screen through
/// the semantics (accessibility) tree via `ax-find --name <label>`.
/// Every interactive widget below carries a STABLE `Semantics(label:)`
/// token (e.g. `search-input`, `search-submit`, `result-hit`). Those
/// tokens are the selector contract — don't rename them casually.
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../client/escurel_client.dart';
import '../client/models.dart';
import '../state/providers.dart';

/// Stable semantics tokens shared with `scripts/verify-demo.sh`.
class DemoKeys {
  static const searchInput = 'search-input';
  static const searchSubmit = 'search-submit';
  static const resultHit = 'result-hit';

  static const authorContent = 'author-content';
  static const authorValidate = 'author-validate';
  static const authorSave = 'author-save';
  static const authorResult = 'author-result';

  static const chatGroup = 'chat-group';
  static const chatContent = 'chat-content';
  static const chatAppend = 'chat-append';
  static const chatList = 'chat-list';
  static const chatMessage = 'chat-message';

  static const opsRefresh = 'ops-refresh';
  static const opsQuota = 'ops-quota';
  static const opsAudit = 'ops-audit';
}

class DemoScreen extends StatelessWidget {
  const DemoScreen({super.key});

  @override
  Widget build(BuildContext context) {
    return DefaultTabController(
      length: 4,
      child: Scaffold(
        appBar: AppBar(
          title: const Text('escurel — capability demo'),
          bottom: const TabBar(
            tabs: [
              Tab(text: 'Search', key: ValueKey('tab-search')),
              Tab(text: 'Author', key: ValueKey('tab-author')),
              Tab(text: 'Chat', key: ValueKey('tab-chat')),
              Tab(text: 'Ops', key: ValueKey('tab-ops')),
            ],
          ),
        ),
        body: const TabBarView(
          children: [
            _SearchPanel(),
            _AuthorPanel(),
            _ChatPanel(),
            _OpsPanel(),
          ],
        ),
      ),
    );
  }
}

/// A labelled, read-only control: wraps [child] in a Semantics node
/// whose accessible name is [label] so rodney's `ax-find --name`
/// reaches it. Use for inputs (the inner <input> carries the label)
/// and status/result text. NOT for buttons — see [_ActionButton].
class _Labelled extends StatelessWidget {
  const _Labelled({required this.label, required this.child, this.textField = false});
  final String label;
  final bool textField;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    return Semantics(
      label: label,
      textField: textField,
      identifier: label,
      container: true,
      explicitChildNodes: true,
      child: child,
    );
  }
}

/// An action button with a STABLE semantics identifier/label AND its
/// own `onTap` wired to the same callback. The own-`onTap` is the
/// crucial bit: Flutter web turns it into an actionable semantics
/// node, so a screen-reader / rodney tap (`flt-semantics[...] click`)
/// actually fires [onTap]. `excludeSemantics` collapses the visual
/// button's intrinsic semantics into this one node, so there's
/// exactly one actionable node named [label].
class _ActionButton extends StatelessWidget {
  const _ActionButton({
    required this.label,
    required this.text,
    required this.onTap,
    this.filled = true,
  });
  final String label;
  final String text;
  final VoidCallback onTap;
  final bool filled;

  @override
  Widget build(BuildContext context) {
    final button = filled
        ? ElevatedButton(onPressed: onTap, child: Text(text))
        : OutlinedButton(onPressed: onTap, child: Text(text));
    return Semantics(
      identifier: label,
      label: label,
      button: true,
      onTap: onTap,
      excludeSemantics: true,
      child: button,
    );
  }
}

// ── Search ──────────────────────────────────────────────────────

class _SearchPanel extends ConsumerStatefulWidget {
  const _SearchPanel();
  @override
  ConsumerState<_SearchPanel> createState() => _SearchPanelState();
}

class _SearchPanelState extends ConsumerState<_SearchPanel> {
  final _q = TextEditingController(text: 'acme');
  List<SearchHit> _hits = const [];
  String _status = 'idle';

  Future<void> _run() async {
    setState(() => _status = 'searching');
    try {
      final res = await ref.read(escurelClientProvider).search(q: _q.text, k: 10);
      setState(() {
        _hits = res.hits;
        _status = '${res.hits.length} hits';
      });
    } catch (e) {
      setState(() => _status = 'error: $e');
    }
  }

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.all(16),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          _Labelled(
            label: DemoKeys.searchInput,
            textField: true,
            child: TextField(
              controller: _q,
              decoration: const InputDecoration(labelText: 'Hybrid search (vector + FTS, RRF-fused)'),
              onSubmitted: (_) => _run(),
            ),
          ),
          const SizedBox(height: 8),
          _ActionButton(label: DemoKeys.searchSubmit, text: 'Search', onTap: _run),
          const SizedBox(height: 8),
          Semantics(label: 'search-status', child: Text(_status)),
          const Divider(),
          Expanded(
            child: ListView.builder(
              itemCount: _hits.length,
              itemBuilder: (context, i) {
                final h = _hits[i];
                return _Labelled(
                  label: DemoKeys.resultHit,
                  child: ListTile(
                    title: Text('${h.skill} · ${h.pageId}'),
                    subtitle: Text(h.snippet ?? ''),
                    trailing: Text(h.score.toStringAsFixed(3)),
                  ),
                );
              },
            ),
          ),
        ],
      ),
    );
  }
}

// ── Author (validate + update_page) ─────────────────────────────

class _AuthorPanel extends ConsumerStatefulWidget {
  const _AuthorPanel();
  @override
  ConsumerState<_AuthorPanel> createState() => _AuthorPanelState();
}

class _AuthorPanelState extends ConsumerState<_AuthorPanel> {
  final _pageId = TextEditingController(text: 'markdown/instances/note/demo.md');
  final _content = TextEditingController(
    text: '---\ntype: instance\nskill: note\nid: demo\ntitle: Demo\n---\n\n# Demo\n\nAuthored from the capability demo.\n',
  );
  String _result = 'idle';

  Future<void> _validate() async {
    try {
      final v = await ref.read(escurelClientProvider).validate(_content.text, asPageId: _pageId.text);
      setState(() => _result = v.isOk
          ? 'valid (${v.issues.length} issues)'
          : 'invalid: ${v.issues.map((i) => i.message).join("; ")}');
    } catch (e) {
      setState(() => _result = 'error: $e');
    }
  }

  Future<void> _save() async {
    try {
      final u = await ref.read(escurelClientProvider).updatePage(_pageId.text, _content.text);
      setState(() => _result = u.ok ? 'saved (${u.newVersion ?? "ok"})' : 'rejected');
    } catch (e) {
      setState(() => _result = 'error: $e');
    }
  }

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.all(16),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          TextField(controller: _pageId, decoration: const InputDecoration(labelText: 'page_id')),
          const SizedBox(height: 8),
          _Labelled(
            label: DemoKeys.authorContent,
            textField: true,
            child: TextField(
              controller: _content,
              maxLines: 8,
              decoration: const InputDecoration(labelText: 'markdown', border: OutlineInputBorder()),
            ),
          ),
          const SizedBox(height: 8),
          Row(children: [
            _ActionButton(label: DemoKeys.authorValidate, text: 'Validate', onTap: _validate, filled: false),
            const SizedBox(width: 8),
            _ActionButton(label: DemoKeys.authorSave, text: 'Save (update_page)', onTap: _save),
          ]),
          const SizedBox(height: 12),
          _Labelled(label: DemoKeys.authorResult, child: Text(_result)),
        ],
      ),
    );
  }
}

// ── Chat (append_message + list_messages) ───────────────────────

class _ChatPanel extends ConsumerStatefulWidget {
  const _ChatPanel();
  @override
  ConsumerState<_ChatPanel> createState() => _ChatPanelState();
}

class _ChatPanelState extends ConsumerState<_ChatPanel> {
  final _group = TextEditingController(text: 'room-demo');
  final _content = TextEditingController(text: 'hello from the demo');
  List<ChatMessage> _messages = const [];
  String _status = 'idle';

  Future<void> _append() async {
    try {
      await ref.read(escurelClientProvider).appendMessage(
            chatGroupId: _group.text,
            role: 'user',
            content: _content.text,
          );
      _content.clear();
      await _list();
    } catch (e) {
      setState(() => _status = 'error: $e');
    }
  }

  Future<void> _list() async {
    try {
      final page = await ref.read(escurelClientProvider).listMessages(_group.text, direction: 'asc');
      setState(() {
        _messages = page.messages;
        _status = '${page.messages.length} messages';
      });
    } catch (e) {
      setState(() => _status = 'error: $e');
    }
  }

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.all(16),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          _Labelled(
            label: DemoKeys.chatGroup,
            textField: true,
            child: TextField(controller: _group, decoration: const InputDecoration(labelText: 'chat_group_id (opaque)')),
          ),
          const SizedBox(height: 8),
          Row(children: [
            Expanded(
              child: _Labelled(
                label: DemoKeys.chatContent,
                textField: true,
                child: TextField(controller: _content, decoration: const InputDecoration(labelText: 'message')),
              ),
            ),
            const SizedBox(width: 8),
            _ActionButton(label: DemoKeys.chatAppend, text: 'Append', onTap: _append),
            const SizedBox(width: 8),
            _ActionButton(label: DemoKeys.chatList, text: 'Refresh', onTap: _list, filled: false),
          ]),
          const SizedBox(height: 8),
          Semantics(label: 'chat-status', child: Text(_status)),
          const Divider(),
          Expanded(
            child: ListView.builder(
              itemCount: _messages.length,
              itemBuilder: (context, i) {
                final m = _messages[i];
                return _Labelled(
                  label: DemoKeys.chatMessage,
                  child: ListTile(
                    dense: true,
                    leading: Text(m.role),
                    title: Text(m.content),
                    subtitle: Text('${m.ts} · embedded=${m.embedded}'),
                  ),
                );
              },
            ),
          ),
        ],
      ),
    );
  }
}

// ── Ops (admin_quota + admin_audit) ─────────────────────────────

class _OpsPanel extends ConsumerStatefulWidget {
  const _OpsPanel();
  @override
  ConsumerState<_OpsPanel> createState() => _OpsPanelState();
}

class _OpsPanelState extends ConsumerState<_OpsPanel> {
  QuotaSnapshot? _quota;
  AuditDrift? _audit;
  String _status = 'idle';

  Future<void> _refresh() async {
    setState(() => _status = 'loading');
    try {
      final c = ref.read(escurelClientProvider);
      final q = await c.adminQuota();
      final a = await c.adminAudit();
      setState(() {
        _quota = q;
        _audit = a;
        _status = 'loaded';
      });
    } catch (e) {
      setState(() => _status = 'error: $e');
    }
  }

  @override
  Widget build(BuildContext context) {
    final q = _quota;
    final a = _audit;
    return Padding(
      padding: const EdgeInsets.all(16),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          _ActionButton(label: DemoKeys.opsRefresh, text: 'Load ops (admin)', onTap: _refresh),
          const SizedBox(height: 8),
          Semantics(label: 'ops-status', child: Text(_status)),
          const SizedBox(height: 12),
          if (q != null)
            _Labelled(
              label: DemoKeys.opsQuota,
              child: Text('quota — queries:${q.queriesRemaining} writes:${q.writesRemaining} '
                  'embeds:${q.embedsRemaining} sessions:${q.concurrentSessionsInUse}'),
            ),
          const SizedBox(height: 8),
          if (a != null)
            _Labelled(
              label: DemoKeys.opsAudit,
              child: Text('audit — ${a.isClean ? "clean" : "drift"}: '
                  'md_not_in_db=${a.markdownNotInDuckdb.length} '
                  'db_not_in_md=${a.indexedButNoMarkdown.length}'),
            ),
        ],
      ),
    );
  }
}
