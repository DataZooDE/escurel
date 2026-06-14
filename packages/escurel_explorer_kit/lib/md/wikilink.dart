/// Dart mirror of `escurel-md`'s typed wikilink parser.
///
/// Grammar: `[[skill::id#anchor@version|alias]]`. All segments
/// except the link target are optional; a bare `[[id]]` is an
/// id-only reference. Fenced code blocks (` ``` `) and inline code
/// spans (`` ` ``) are stripped before scanning.
library;

/// One wikilink occurrence, decomposed.
class WikilinkRef {
  const WikilinkRef({
    this.skill,
    this.id,
    this.anchor,
    this.version,
    this.alias,
  });

  final String? skill;
  final String? id;
  final String? anchor;
  final String? version;
  final String? alias;

  /// Render back to wikilink source for round-trip use.
  String toMarkup() {
    final target = [
      if (skill != null) '$skill::',
      if (id != null) id,
    ].whereType<String>().join();
    final suffixes = [
      if (anchor != null) '#$anchor',
      if (version != null) '@$version',
      if (alias != null) '|$alias',
    ].join();
    return '[[$target$suffixes]]';
  }

  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      other is WikilinkRef &&
          skill == other.skill &&
          id == other.id &&
          anchor == other.anchor &&
          version == other.version &&
          alias == other.alias;

  @override
  int get hashCode => Object.hash(skill, id, anchor, version, alias);

  @override
  String toString() =>
      'WikilinkRef(skill: $skill, id: $id, anchor: $anchor, version: $version, alias: $alias)';
}

final RegExp _wikilinkRe = RegExp(r'\[\[([^\[\]\r\n]+?)\]\]');
final RegExp _inlineCodeRe = RegExp(r'`[^`\n]*`');

/// Parse every wikilink in [markdown] in document order, skipping
/// wikilinks inside fenced code blocks or inline code spans.
List<WikilinkRef> parseWikilinks(String markdown) {
  final stripped = _stripCodeRegions(markdown);
  return _wikilinkRe
      .allMatches(stripped)
      .map((m) => _parseOne(m.group(1)!))
      .whereType<WikilinkRef>()
      .toList(growable: false);
}

WikilinkRef? _parseOne(String content) {
  final (target1, alias) = _splitFirst(content, '|');
  final (target2, version) = _splitFirst(target1, '@');
  final (target3, anchor) = _splitFirst(target2, '#');

  String? skill;
  String? id;
  final sepIdx = target3.indexOf('::');
  if (sepIdx >= 0) {
    skill = _someIfNonempty(target3.substring(0, sepIdx).trim());
    id = _someIfNonempty(target3.substring(sepIdx + 2).trim());
  } else {
    id = _someIfNonempty(target3.trim());
  }

  if (skill == null && id == null) return null;

  return WikilinkRef(
    skill: skill,
    id: id,
    anchor: anchor != null ? _someIfNonempty(anchor.trim()) : null,
    version: version != null ? _someIfNonempty(version.trim()) : null,
    alias: alias != null ? _someIfNonempty(alias.trim()) : null,
  );
}

(String, String?) _splitFirst(String s, String sep) {
  final idx = s.indexOf(sep);
  if (idx < 0) return (s.trim(), null);
  return (s.substring(0, idx).trim(), s.substring(idx + 1).trim());
}

String? _someIfNonempty(String s) => s.isEmpty ? null : s;

String _stripCodeRegions(String input) {
  final blanked = StringBuffer();
  var inFence = false;
  for (final line in _linesInclusive(input)) {
    final isFenceMarker = line.trimLeft().startsWith('```');
    if (isFenceMarker || inFence) {
      for (final unit in line.runes) {
        blanked.writeCharCode(unit == 0x0A ? 0x0A : 0x20);
      }
      if (isFenceMarker) inFence = !inFence;
    } else {
      blanked.write(line);
    }
  }
  return blanked.toString().replaceAllMapped(_inlineCodeRe, (m) => ' ' * m[0]!.length);
}

Iterable<String> _linesInclusive(String input) sync* {
  var start = 0;
  while (start < input.length) {
    final nl = input.indexOf('\n', start);
    if (nl < 0) {
      yield input.substring(start);
      return;
    }
    yield input.substring(start, nl + 1);
    start = nl + 1;
  }
}
