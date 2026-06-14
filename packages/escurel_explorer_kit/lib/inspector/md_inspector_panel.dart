import 'package:flutter/material.dart';

import '../md/frontmatter.dart' as md;
import '../md/wikilink.dart';
import '../theme/app_theme.dart';

/// Paste any escurel markdown into the left side; the right side
/// shows the parsed structure — frontmatter as a key/value table
/// and the outgoing wikilinks (from body + frontmatter values) as
/// chips. Pure client-side; never touches the backend.
class MdInspectorPanel extends StatefulWidget {
  const MdInspectorPanel({super.key});

  @override
  State<MdInspectorPanel> createState() => _MdInspectorPanelState();
}

class _MdInspectorPanelState extends State<MdInspectorPanel> {
  late final TextEditingController _ctrl;

  static const _seedSample = '''---
type: instance
skill: opportunity
id: hoffmann-pilot
customer: [[customer::muenchner-pharma]]
champion: [[contact::hoffmann]]
value_eur: 60000
status: negotiating
---

# Münchner Pharma — pilot

Hoffmann is the champion. See [[engagement::hoffmann-intro]] for
the first conversation and [[lead::hoffmann-followup]] for the
qualification walk.
''';

  @override
  void initState() {
    super.initState();
    _ctrl = TextEditingController(text: _seedSample);
  }

  @override
  void dispose() {
    _ctrl.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return Row(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Expanded(
          child: Container(
            color: kSurfaceContainerLowest,
            padding: const EdgeInsets.all(12),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  'Markdown input',
                  style: Theme.of(context).textTheme.titleSmall,
                ),
                const SizedBox(height: 6),
                Expanded(
                  child: TextField(
                    key: const ValueKey('md_inspector.input'),
                    controller: _ctrl,
                    maxLines: null,
                    expands: true,
                    onChanged: (_) => setState(() {}),
                    style: const TextStyle(fontFamily: 'monospace', fontSize: 12),
                    decoration: const InputDecoration(
                      isDense: true,
                      contentPadding: EdgeInsets.all(8),
                      border: OutlineInputBorder(),
                    ),
                  ),
                ),
              ],
            ),
          ),
        ),
        const VerticalDivider(width: 1),
        Expanded(
          child: Container(
            color: kSurface,
            padding: const EdgeInsets.all(12),
            child: _ParsedView(source: _ctrl.text),
          ),
        ),
      ],
    );
  }
}

class _ParsedView extends StatelessWidget {
  const _ParsedView({required this.source});

  final String source;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;

    md.Page? page;
    String? error;
    try {
      page = md.parse(source);
    } on md.ParseException catch (e) {
      error = e.message;
    } catch (e) {
      error = e.toString();
    }

    if (error != null) {
      return Center(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(
              'Parse error',
              style: text.titleSmall?.copyWith(color: kError),
            ),
            const SizedBox(height: 4),
            Text(error, style: text.bodySmall?.copyWith(color: kError)),
          ],
        ),
      );
    }

    final p = page!;
    // parseWikilinks returns a fixed-length list; copy into a growable
    // one so we can append frontmatter-derived refs.
    final wikilinks = <WikilinkRef>[...parseWikilinks(p.body)];
    for (final v in p.frontmatter.fields.values) {
      if (v != null) wikilinks.addAll(parseWikilinks(v.toString()));
    }

    return SingleChildScrollView(
      key: const ValueKey('md_inspector.output'),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text('Frontmatter', style: text.titleSmall),
          const SizedBox(height: 6),
          _kv('type', p.frontmatter.pageType.name),
          for (final entry in p.frontmatter.fields.entries)
            if (entry.key != 'type') _kv(entry.key, entry.value?.toString() ?? ''),
          const SizedBox(height: 16),
          Text('Outgoing wikilinks (${wikilinks.length})', style: text.titleSmall),
          const SizedBox(height: 6),
          if (wikilinks.isEmpty)
            Text('none', style: text.bodySmall?.copyWith(color: kOnSurfaceVariant))
          else
            Wrap(
              spacing: 6,
              runSpacing: 4,
              children: [for (final w in wikilinks) _WikilinkChip(markup: w.toMarkup())],
            ),
          const SizedBox(height: 16),
          Text(
            'Body — ${p.body.length} chars, ${p.body.split('\n').length} lines',
            style: text.titleSmall,
          ),
        ],
      ),
    );
  }

  Widget _kv(String key, String value) {
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 2),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            width: 140,
            child: Text(key, style: const TextStyle(color: kOnSurfaceVariant, fontSize: 12)),
          ),
          Expanded(
            child: Text(value, style: const TextStyle(fontSize: 12)),
          ),
        ],
      ),
    );
  }
}

class _WikilinkChip extends StatelessWidget {
  const _WikilinkChip({required this.markup});

  final String markup;

  @override
  Widget build(BuildContext context) {
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
      decoration: BoxDecoration(
        color: kSurfaceContainer,
        borderRadius: BorderRadius.circular(4),
        border: Border.all(color: kOutlineVariant),
      ),
      child: Text(
        markup,
        style: Theme.of(context).textTheme.labelSmall?.copyWith(color: kPrimary),
      ),
    );
  }
}
