/// TOP bar (M7) — hybrid `search` pinned above both views. On submit it
/// runs the real `search` tool and pins the top hit as the focused
/// entity (right pane). Distinct from `capture` (bottom): search *finds*
/// an existing memory, capture *appends* a new event.
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../state/providers.dart';
import '../theme/app_theme.dart';

class WorkspaceSearchBar extends ConsumerStatefulWidget {
  const WorkspaceSearchBar({super.key});
  @override
  ConsumerState<WorkspaceSearchBar> createState() => _SearchBarState();
}

class _SearchBarState extends ConsumerState<WorkspaceSearchBar> {
  final _controller = TextEditingController();
  String _status = '';

  Future<void> _send() async {
    final q = _controller.text.trim();
    if (q.isEmpty) return;
    setState(() => _status = 'searching…');
    try {
      final res = await ref.read(escurelClientProvider).search(q: q, k: 10);
      if (res.hits.isEmpty) {
        setState(() => _status = 'no hits for "$q"');
        return;
      }
      // A search is a fresh jump — drop any link-following trail.
      ref.read(currentPageIdProvider.notifier).state = res.hits.first.pageId;
      clearNavHistory(ref);
      setState(() => _status = '${res.hits.length} hits → ${res.hits.first.skill}');
    } catch (e) {
      setState(() => _status = 'error: $e');
    }
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    return Container(
      decoration: const BoxDecoration(
        color: kSurfaceContainerLowest,
        border: Border(bottom: BorderSide(color: kOutlineVariant)),
      ),
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
      child: Row(
        children: [
          const Icon(Icons.search, size: 18, color: kOutline),
          const SizedBox(width: 10),
          Expanded(
            child: Semantics(
              label: 'search-input',
              textField: true,
              container: true,
              explicitChildNodes: true,
              child: TextField(
                controller: _controller,
                onSubmitted: (_) => _send(),
                decoration: const InputDecoration(
                  isDense: true,
                  border: InputBorder.none,
                  hintText: 'search memory…',
                ),
              ),
            ),
          ),
          if (_status.isNotEmpty) ...[
            Semantics(
              label: 'search-status',
              child: Text(_status, style: text.labelSmall?.copyWith(color: kOutline)),
            ),
            const SizedBox(width: 12),
          ],
          Semantics(
            label: 'search-send',
            button: true,
            onTap: _send,
            excludeSemantics: true,
            child: FilledButton(
              onPressed: _send,
              style: FilledButton.styleFrom(visualDensity: VisualDensity.compact),
              child: const Text('search'),
            ),
          ),
        ],
      ),
    );
  }
}
