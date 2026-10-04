@TestOn('vm')
library;

import 'dart:convert';
import 'dart:io';

import 'package:dio/dio.dart';
import 'package:escurel_explorer_kit/client/escurel_client.dart';
import 'package:escurel_explorer_kit/client/http_escurel_client.dart';
import 'package:flutter_test/flutter_test.dart';

// The wire, as a real gateway and runner sent it: crates/escurel-types/tests/golden/*.json, captured by
// scripts/refresh-golden.sh. The Rust types and the TypeScript extension decode the same files, so a wire
// change cannot drift between the three hand-copied implementations unnoticed.
Directory _goldenDir() {
  var dir = Directory.current;
  for (var i = 0; i < 6; i += 1) {
    final candidate = Directory('${dir.path}/crates/escurel-types/tests/golden');
    if (candidate.existsSync()) return candidate;
    dir = dir.parent;
  }
  throw StateError('crates/escurel-types/tests/golden not found from ${Directory.current.path}');
}

Map<String, dynamic> _golden(String name) =>
    jsonDecode(File('${_goldenDir().path}/$name.json').readAsStringSync())
        as Map<String, dynamic>;

/// Serves one tools/call result for any tool: either the golden `structuredContent` wrapped the way the
/// gateway wraps it, or (for `*_result` files) the whole result as captured.
class _Gateway {
  _Gateway(this._server, this.baseUrl);
  final HttpServer _server;
  final String baseUrl;
  Map<String, dynamic> result = const {};

  static Future<_Gateway> start() async {
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    final gw = _Gateway(server, 'http://${server.address.host}:${server.port}');
    server.listen((req) async {
      final body =
          jsonDecode(await utf8.decoder.bind(req).join()) as Map<String, dynamic>;
      req.response.headers.contentType = ContentType.json;
      req.response.write(
        jsonEncode({'jsonrpc': '2.0', 'id': body['id'], 'result': gw.result}),
      );
      await req.response.close();
    });
    return gw;
  }

  void serveStructured(Map<String, dynamic> structured) => result = {
    'content': [
      {'type': 'text', 'text': 'golden'},
    ],
    'structuredContent': structured,
  };

  Future<void> stop() => _server.close(force: true);
}

void main() {
  late _Gateway gw;
  late EscurelClient client;

  setUp(() async {
    gw = await _Gateway.start();
    client = HttpEscurelClient(baseUrl: gw.baseUrl, dio: Dio());
  });

  tearDown(() async {
    client.close();
    await gw.stop();
  });

  test('list_skills decodes every skill of a real gateway', () async {
    final golden = _golden('list_skills');
    gw.serveStructured(golden);
    final skills = await client.listSkills();
    expect(skills.map((s) => s.id), (golden['skills'] as List).map((s) => (s as Map)['id']));
    for (final s in skills) {
      expect(s.id, isNotEmpty);
      expect(s.requiredFrontmatter, isA<List<String>>());
    }
  });

  test('expand decodes a real instance page', () async {
    final golden = _golden('expand_instance');
    gw.serveStructured(golden);
    final page = golden['page'] as Map<String, dynamic>;
    final e = await client.expand(page['page_id'] as String);
    expect(e.pageId, page['page_id']);
    expect(e.skill, page['skill']);
    expect(e.frontmatter, isNotEmpty);
  });

  test('list_instances decodes a real page and its cursor', () async {
    final golden = _golden('list_instances');
    gw.serveStructured(golden);
    final p = await client.listInstances('customer-order', limit: 2);
    expect(p.instances.length, (golden['instances'] as List).length);
    expect(p.instances.first.id, isNotEmpty);
  });

  test('a held write is not mistaken for a landed one', () async {
    gw.serveStructured(_golden('update_page_held'));
    final r = await client.updatePage('markdown/instances/x.md', 'content');
    expect(r.heldForReview, isTrue);
  });

  test('validate reports ok:false issues as data', () async {
    gw.result = _golden('validate_result');
    final v = await client.validate('x', asPageId: 'markdown/skills/x.md');
    expect(v.issues, isNotEmpty);
    expect(v.isOk, isFalse);
  });

  test('run events decode as events with their lifecycle titles', () async {
    final golden = _golden('events_run');
    gw.serveStructured(golden);
    final page = await client.listEvents('markdown/instances/x.md');
    final titles = page.events.map((e) => e.title).toSet();
    expect(titles, containsAll(['run-started', 'run-finished']));
  });
}
