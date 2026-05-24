/// Dart mirror of `escurel-md`'s frontmatter parser.
///
/// Kept deliberately small so the UI can render `[[wikilink]]` pills
/// and skill/instance pages offline (no round-trip to the server)
/// in fixture mode. The grammar is identical to the Rust parser:
///
/// ```text
/// ---
/// type: skill        # required, "skill" or "instance"
/// id: customer
/// ... other yaml ...
/// ---
///
/// # Body markdown
/// ```
library;

import 'package:yaml/yaml.dart';

/// The two kinds of pages an escurel tenant carries.
enum PageType { skill, instance }

/// Parsed frontmatter — the typed `type:` plus the raw YAML mapping
/// for arbitrary projection by consumers.
class Frontmatter {
  const Frontmatter({required this.pageType, required this.fields});

  final PageType pageType;
  final Map<String, dynamic> fields;
}

/// A page split into frontmatter and the body markdown that follows.
class Page {
  const Page({required this.frontmatter, required this.body});

  final Frontmatter frontmatter;
  final String body;
}

/// Error class for frontmatter parsing failures.
class ParseException implements Exception {
  const ParseException(this.message);

  final String message;

  @override
  String toString() => 'ParseException: $message';
}

/// Parse a markdown file into [Frontmatter] + body.
///
/// Throws [ParseException] when the input is missing a frontmatter
/// block, the YAML is malformed, or the required `type:` field is
/// absent or not `skill`/`instance`.
Page parse(String input) {
  const opener = '---\n';
  if (!input.startsWith(opener)) {
    throw const ParseException('missing frontmatter: input does not start with "---"');
  }
  final afterOpen = input.substring(opener.length);

  final closeIdx = _findClosingDelimiter(afterOpen);
  if (closeIdx == null) {
    throw const ParseException('unterminated frontmatter: no closing "---" found');
  }

  final yamlBlock = afterOpen.substring(0, closeIdx.yamlEnd);
  final body = afterOpen.substring(closeIdx.bodyStart);

  final loaded = loadYaml(yamlBlock);
  if (loaded is! YamlMap) {
    throw const ParseException('frontmatter must be a YAML mapping at the top level');
  }
  final fields = _normaliseMap(loaded);

  final typeRaw = fields['type'];
  final pageType = switch (typeRaw) {
    'skill' => PageType.skill,
    'instance' => PageType.instance,
    _ => throw const ParseException(
      'frontmatter missing or invalid "type" (expected "skill" or "instance")',
    ),
  };

  return Page(
    frontmatter: Frontmatter(pageType: pageType, fields: fields),
    body: body,
  );
}

class _CloseLoc {
  const _CloseLoc({required this.yamlEnd, required this.bodyStart});
  final int yamlEnd;
  final int bodyStart;
}

/// Locate the closing `---` line. Returns the offset of the last
/// newline before the closer (yamlEnd, exclusive) and the offset
/// where body content begins (bodyStart, after the closer's newline
/// or end-of-input).
_CloseLoc? _findClosingDelimiter(String input) {
  var cursor = 0;
  while (cursor < input.length) {
    final lineEnd = input.indexOf('\n', cursor);
    final endOfLine = lineEnd == -1 ? input.length : lineEnd;
    final line = input.substring(cursor, endOfLine);
    if (line == '---') {
      final yamlEnd = cursor == 0 ? 0 : cursor - 1;
      final bodyStart = lineEnd == -1 ? input.length : lineEnd + 1;
      return _CloseLoc(yamlEnd: yamlEnd, bodyStart: bodyStart);
    }
    if (lineEnd == -1) {
      break;
    }
    cursor = lineEnd + 1;
  }
  return null;
}

Map<String, dynamic> _normaliseMap(YamlMap node) {
  final out = <String, dynamic>{};
  for (final entry in node.entries) {
    out[entry.key.toString()] = _normaliseValue(entry.value);
  }
  return out;
}

dynamic _normaliseValue(dynamic value) {
  if (value is YamlMap) return _normaliseMap(value);
  if (value is YamlList) return value.map(_normaliseValue).toList();
  return value;
}
