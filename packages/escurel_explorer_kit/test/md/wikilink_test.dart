import 'package:escurel_explorer_kit/md/wikilink.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  group('parseWikilinks', () {
    test('parses a bare id-only link', () {
      final refs = parseWikilinks('see [[hoffmann]] later');
      expect(refs, hasLength(1));
      expect(refs.single, const WikilinkRef(id: 'hoffmann'));
    });

    test('parses a typed skill::id link', () {
      final refs = parseWikilinks('hi [[contact::hoffmann]]');
      expect(refs.single, const WikilinkRef(skill: 'contact', id: 'hoffmann'));
    });

    test('parses anchor, version, alias suffixes', () {
      final refs = parseWikilinks('[[contact::hoffmann#background@v3|the champ]]');
      expect(
        refs.single,
        const WikilinkRef(
          skill: 'contact',
          id: 'hoffmann',
          anchor: 'background',
          version: 'v3',
          alias: 'the champ',
        ),
      );
    });

    test('returns links in document order', () {
      final refs = parseWikilinks('a [[a]] b [[b]] c [[c]] d');
      expect(refs.map((r) => r.id).toList(), ['a', 'b', 'c']);
    });

    test('skips wikilinks inside a fenced code block', () {
      final refs = parseWikilinks('''
visible [[a]]
```
hidden [[b]]
```
visible [[c]]
''');
      expect(refs.map((r) => r.id).toList(), ['a', 'c']);
    });

    test('skips wikilinks inside inline code spans', () {
      final refs = parseWikilinks('outer [[a]] and `inline [[b]] hidden` and [[c]]');
      expect(refs.map((r) => r.id).toList(), ['a', 'c']);
    });

    test('returns empty list when no links present', () {
      expect(parseWikilinks('no links here'), isEmpty);
    });

    test('discards a malformed empty link', () {
      expect(parseWikilinks('[[]] and [[ ]]'), isEmpty);
    });

    test('trims whitespace around segments', () {
      final refs = parseWikilinks('[[ contact :: hoffmann # bg | Alias ]]');
      expect(
        refs.single,
        const WikilinkRef(skill: 'contact', id: 'hoffmann', anchor: 'bg', alias: 'Alias'),
      );
    });

    group('toMarkup', () {
      test('round-trips a typed link with all suffixes', () {
        const ref = WikilinkRef(
          skill: 'contact',
          id: 'hoffmann',
          anchor: 'bg',
          version: 'v2',
          alias: 'A',
        );
        expect(ref.toMarkup(), '[[contact::hoffmann#bg@v2|A]]');
      });

      test('round-trips a bare id-only link', () {
        const ref = WikilinkRef(id: 'hoffmann');
        expect(ref.toMarkup(), '[[hoffmann]]');
      });
    });
  });
}
