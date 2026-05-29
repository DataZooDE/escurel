/// Bottom command bar for the CRM workspace. Mirrors the mockup's
/// "TEXT  type, or capture from voice / image / live → send". For now
/// it runs a hybrid search and focuses the top hit; voice/image/live
/// are visual affordances pending backend support.
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../state/providers.dart';
import '../theme/app_theme.dart';

class CommandBar extends ConsumerStatefulWidget {
  const CommandBar({super.key});
  @override
  ConsumerState<CommandBar> createState() => _CommandBarState();
}

class _CommandBarState extends ConsumerState<CommandBar> {
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
      ref.read(currentPageIdProvider.notifier).state = res.hits.first.pageId;
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
        border: Border(top: BorderSide(color: kOutlineVariant)),
      ),
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 10),
      child: Row(
        children: [
          Text('TEXT', style: text.labelSmall?.copyWith(color: kOutline)),
          const SizedBox(width: 12),
          Expanded(
            child: Semantics(
              label: 'command-input',
              textField: true,
              container: true,
              explicitChildNodes: true,
              child: TextField(
                controller: _controller,
                onSubmitted: (_) => _send(),
                decoration: const InputDecoration(
                  isDense: true,
                  border: InputBorder.none,
                  hintText: 'type, or capture from voice / image / live →',
                ),
              ),
            ),
          ),
          if (_status.isNotEmpty) ...[
            Semantics(
              label: 'command-status',
              child: Text(_status, style: text.labelSmall?.copyWith(color: kOutline)),
            ),
            const SizedBox(width: 12),
          ],
          for (final cap in const ['voice', 'image', 'live'])
            Padding(
              padding: const EdgeInsets.only(right: 8),
              child: _Pill(label: cap),
            ),
          Semantics(
            label: 'command-send',
            button: true,
            onTap: _send,
            excludeSemantics: true,
            child: FilledButton.icon(
              onPressed: _send,
              icon: const Icon(Icons.send, size: 16),
              label: const Text('send'),
            ),
          ),
        ],
      ),
    );
  }
}

class _Pill extends StatelessWidget {
  const _Pill({required this.label});
  final String label;
  @override
  Widget build(BuildContext context) => Container(
        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 5),
        decoration: BoxDecoration(
          color: kSurfaceContainer,
          borderRadius: BorderRadius.circular(999),
        ),
        child: Text(label, style: Theme.of(context).textTheme.labelSmall?.copyWith(color: kOnSurfaceVariant)),
      );
}
