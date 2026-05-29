/// Time scrubber — the bottom-edge control from the mockups. A slider
/// across the corpus's event span drives the global `as_of` cut; the
/// readout shows `T+Nd` (days since the corpus start); the speed chips
/// (1× / 5× / 50× / 500×) set the auto-play cadence and the play/pause
/// button sweeps the cut forward in wall-clock time.
///
/// All it does is set [asOfProvider]; every read provider already passes
/// that down as the backend `as_of`, so scrubbing reshapes the inbox,
/// the skill-wheel, and the reader at once — real time-travel, no faking.
library;

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../state/providers.dart';
import '../theme/app_theme.dart';
import 'crm_providers.dart';

const _speeds = <int>[1, 5, 50, 500];

class TimeScrubber extends ConsumerStatefulWidget {
  const TimeScrubber({super.key});
  @override
  ConsumerState<TimeScrubber> createState() => _TimeScrubberState();
}

class _TimeScrubberState extends ConsumerState<TimeScrubber> {
  int _speed = 1;
  Timer? _timer;

  @override
  void dispose() {
    _timer?.cancel();
    super.dispose();
  }

  bool get _playing => _timer != null;

  void _togglePlay(({DateTime start, DateTime end}) range) {
    if (_playing) {
      _timer?.cancel();
      setState(() => _timer = null);
      return;
    }
    // From the present, a fresh play restarts at the corpus start.
    if (ref.read(asOfProvider) == null) {
      ref.read(asOfProvider.notifier).state = range.start;
    }
    setState(() {
      _timer = Timer.periodic(const Duration(milliseconds: 250), (_) {
        final span = range.end.difference(range.start);
        // One tick advances 1% of the span × the speed multiplier.
        final stepMs = (span.inMilliseconds * 0.01 * _speed).round();
        final cur = ref.read(asOfProvider) ?? range.start;
        final next = cur.add(Duration(milliseconds: stepMs));
        if (!next.isBefore(range.end)) {
          ref.read(asOfProvider.notifier).state = null; // snap to present
          _timer?.cancel();
          setState(() => _timer = null);
        } else {
          ref.read(asOfProvider.notifier).state = next;
        }
      });
    });
  }

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    final rangeAsync = ref.watch(corpusRangeProvider);
    final asOf = ref.watch(asOfProvider);

    return Semantics(
      label: 'time-scrubber',
      container: true,
      explicitChildNodes: true,
      child: Container(
        decoration: const BoxDecoration(
          color: kSurfaceContainerLow,
          border: Border(top: BorderSide(color: kOutlineVariant)),
        ),
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 6),
        child: rangeAsync.maybeWhen(
          data: (range) {
            if (range == null) {
              return const SizedBox.shrink();
            }
            final span = range.end.difference(range.start);
            final cur = asOf ?? range.end;
            final frac = span.inMilliseconds == 0
                ? 1.0
                : (cur.difference(range.start).inMilliseconds / span.inMilliseconds)
                    .clamp(0.0, 1.0);
            final days = cur.difference(range.start).inDays;
            return Row(
              children: [
                IconButton(
                  iconSize: 20,
                  visualDensity: VisualDensity.compact,
                  tooltip: _playing ? 'Pause' : 'Play',
                  onPressed: () => _togglePlay(range),
                  icon: Semantics(
                    label: _playing ? 'time-pause' : 'time-play',
                    button: true,
                    child: Icon(_playing ? Icons.pause : Icons.play_arrow, color: kPrimary),
                  ),
                ),
                SizedBox(
                  width: 64,
                  child: Semantics(
                    label: 'time-readout',
                    excludeSemantics: true,
                    child: Text(
                      asOf == null ? 'now' : 'T+${days}d',
                      style: text.labelMedium?.copyWith(color: kPrimary, fontWeight: FontWeight.w700),
                    ),
                  ),
                ),
                Expanded(
                  child: Slider(
                    value: frac,
                    activeColor: kPrimary,
                    onChanged: (v) {
                      // Dragging to the far right means "the present".
                      if (v >= 0.999) {
                        ref.read(asOfProvider.notifier).state = null;
                      } else {
                        final ms = (span.inMilliseconds * v).round();
                        ref.read(asOfProvider.notifier).state =
                            range.start.add(Duration(milliseconds: ms));
                      }
                    },
                  ),
                ),
                const SizedBox(width: 8),
                for (final s in _speeds) _SpeedChip(
                  speed: s,
                  selected: s == _speed,
                  onTap: () => setState(() => _speed = s),
                ),
              ],
            );
          },
          orElse: () => const SizedBox(height: 36),
        ),
      ),
    );
  }
}

class _SpeedChip extends StatelessWidget {
  const _SpeedChip({required this.speed, required this.selected, required this.onTap});
  final int speed;
  final bool selected;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 2),
      child: Semantics(
        label: 'speed-${speed}x',
        button: true,
        selected: selected,
        onTap: onTap,
        excludeSemantics: true,
        child: InkWell(
          borderRadius: BorderRadius.circular(6),
          onTap: onTap,
          child: Container(
            padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
            decoration: BoxDecoration(
              color: selected ? kPrimary : kSurfaceContainerHigh,
              borderRadius: BorderRadius.circular(6),
            ),
            child: Text(
              '$speed×',
              style: text.labelSmall?.copyWith(
                color: selected ? Colors.white : kOnSurfaceVariant,
                fontWeight: FontWeight.w700,
              ),
            ),
          ),
        ),
      ),
    );
  }
}
