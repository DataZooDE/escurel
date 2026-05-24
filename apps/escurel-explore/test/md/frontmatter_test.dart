import 'package:escurel_explore/md/frontmatter.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  group('parse', () {
    test('parses a valid skill page', () {
      const input = '''---
type: skill
id: customer
description: A buying entity.
required_frontmatter: [name, country]
---

# Customer

Body here.
''';
      final page = parse(input);
      expect(page.frontmatter.pageType, PageType.skill);
      expect(page.frontmatter.fields['id'], 'customer');
      expect(page.frontmatter.fields['description'], 'A buying entity.');
      expect(page.frontmatter.fields['required_frontmatter'], ['name', 'country']);
      expect(page.body, startsWith('\n# Customer\n'));
    });

    test('parses a valid instance page', () {
      const input = '''---
type: instance
skill: contact
id: hoffmann
name: Dr. Hoffmann
---
body
''';
      final page = parse(input);
      expect(page.frontmatter.pageType, PageType.instance);
      expect(page.frontmatter.fields['skill'], 'contact');
      expect(page.body, 'body\n');
    });

    test('throws when the file does not start with ---', () {
      expect(
        () => parse('# title only\n'),
        throwsA(
          isA<ParseException>().having((e) => e.message, 'message', contains('missing frontmatter')),
        ),
      );
    });

    test('throws when the frontmatter block is unterminated', () {
      expect(
        () => parse('---\ntype: skill\nid: x\n'),
        throwsA(
          isA<ParseException>().having(
            (e) => e.message,
            'message',
            contains('unterminated frontmatter'),
          ),
        ),
      );
    });

    test('throws when type is missing', () {
      const input = '''---
id: customer
---
body
''';
      expect(
        () => parse(input),
        throwsA(
          isA<ParseException>().having((e) => e.message, 'message', contains('missing or invalid')),
        ),
      );
    });

    test('throws when type is something other than skill/instance', () {
      const input = '''---
type: page
id: x
---
''';
      expect(() => parse(input), throwsA(isA<ParseException>()));
    });

    test('returns empty body when delimiter is the last line', () {
      const input = '---\ntype: skill\nid: x\n---';
      final page = parse(input);
      expect(page.body, '');
    });

    test('normalises nested yaml maps and lists', () {
      const input = '''---
type: instance
skill: contact
nested:
  one: 1
  two: [a, b, c]
---
''';
      final page = parse(input);
      final nested = page.frontmatter.fields['nested'] as Map;
      expect(nested['one'], 1);
      expect(nested['two'], ['a', 'b', 'c']);
    });
  });
}
