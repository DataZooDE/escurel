/// BOTTOM bar (M7) — `capture` a new event. On submit it calls the real
/// `capture_event` tool, which appends the event to the general inbox
/// (status=`inbox`). An external agent (simulated) later assigns it to
/// an instance via its label-skill; until then it surfaces in the inbox
/// below the event list. Distinct from `search` (top): capture *appends*
/// a new event, search *finds* an existing memory.
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../state/providers.dart';
import '../theme/app_theme.dart';
import 'crm_providers.dart';

class CaptureBar extends ConsumerStatefulWidget {
  const CaptureBar({super.key});
  @override
  ConsumerState<CaptureBar> createState() => _CaptureBarState();
}

class _CaptureBarState extends ConsumerState<CaptureBar> {
  final _controller = TextEditingController();
  String _status = '';
  bool _busy = false;

  Future<void> _send() async {
    final title = _controller.text.trim();
    if (title.isEmpty || _busy) return;
    setState(() {
      _busy = true;
      _status = 'capturing…';
    });
    try {
      final ev = await ref.read(escurelClientProvider).captureEvent(
            source: 'manual',
            mime: 'text/plain',
            labelSkill: 'note',
            title: title,
            body: title,
          );
      _controller.clear();
      // The new event lands in the inbox — refresh it so it appears below.
      ref.invalidate(inboxEventsProvider);
      ref.read(openEventProvider.notifier).state = ev.eventId;
      setState(() => _status = 'captured → inbox');
    } catch (e) {
      setState(() => _status = 'error: $e');
    } finally {
      if (mounted) setState(() => _busy = false);
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
          const Icon(Icons.add_circle_outline, size: 18, color: kPrimary),
          const SizedBox(width: 10),
          Text('CAPTURE', style: text.labelSmall?.copyWith(color: kOutline, letterSpacing: 1)),
          const SizedBox(width: 12),
          Expanded(
            child: Semantics(
              label: 'capture-input',
              textField: true,
              container: true,
              explicitChildNodes: true,
              child: TextField(
                controller: _controller,
                onSubmitted: (_) => _send(),
                decoration: const InputDecoration(
                  isDense: true,
                  border: InputBorder.none,
                  hintText: 'capture a new event →',
                ),
              ),
            ),
          ),
          if (_status.isNotEmpty) ...[
            Semantics(
              label: 'capture-status',
              child: Text(_status, style: text.labelSmall?.copyWith(color: kOutline)),
            ),
            const SizedBox(width: 12),
          ],
          Semantics(
            label: 'capture-send',
            button: true,
            onTap: _send,
            excludeSemantics: true,
            child: FilledButton.icon(
              onPressed: _busy ? null : _send,
              icon: const Icon(Icons.bolt, size: 16),
              label: const Text('capture'),
            ),
          ),
        ],
      ),
    );
  }
}
