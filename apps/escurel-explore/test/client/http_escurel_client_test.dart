@TestOn('vm')
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:dio/dio.dart';
import 'package:escurel_explore/client/errors.dart';
import 'package:escurel_explore/client/escurel_client.dart';
import 'package:escurel_explore/client/http_escurel_client.dart';
import 'package:escurel_explore/client/models.dart';
import 'package:escurel_explore/md/frontmatter.dart' as md;
import 'package:flutter_test/flutter_test.dart';

/// Stands up a real `dart:io` HttpServer on localhost:0 and lets
/// each test inject canned responses keyed by the tool name. True
/// end-to-end: the dio client speaks to a real socket. Proves the
/// MCP/HTTP wire envelope (JSON-RPC 2.0, `method = "tools/call"`,
/// `params = { name, arguments }`) is correctly produced and
/// consumed.
class _MockMcpServer {
  _MockMcpServer._(this._server, this.baseUrl);

  final HttpServer _server;
  final String baseUrl;

  /// Map of tool name → handler that gets the JSON-RPC `arguments`
  /// and returns the `result` map.
  final Map<String, FutureOr<Map<String, dynamic>> Function(Map<String, dynamic>)> toolHandlers = {};

  /// Map of route path → handler for non-MCP endpoints (/healthz, /version).
  final Map<String, FutureOr<Map<String, dynamic>?> Function()> routeHandlers = {};

  /// Pre-built tool error to return on the next call (overrides handler).
  ({int code, String message})? nextError;

  static Future<_MockMcpServer> start() async {
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    final base = 'http://${server.address.host}:${server.port}';
    final mock = _MockMcpServer._(server, base);
    mock._listen();
    return mock;
  }

  void _listen() {
    _server.listen((req) async {
      if (req.method == 'POST' && req.uri.path == '/mcp') {
        final body = jsonDecode(await utf8.decoder.bind(req).join()) as Map<String, dynamic>;
        if (nextError != null) {
          await _respondJson(req, 200, {
            'jsonrpc': '2.0',
            'id': body['id'],
            'error': {'code': nextError!.code, 'message': nextError!.message},
          });
          nextError = null;
          return;
        }
        final params = body['params'] as Map<String, dynamic>;
        final name = params['name'] as String;
        final args = (params['arguments'] as Map<String, dynamic>?) ?? const {};
        final handler = toolHandlers[name];
        if (handler == null) {
          await _respondJson(req, 200, {
            'jsonrpc': '2.0',
            'id': body['id'],
            'error': {'code': 404, 'message': 'no handler for $name'},
          });
          return;
        }
        final result = await handler(args);
        await _respondJson(req, 200, {
          'jsonrpc': '2.0',
          'id': body['id'],
          'result': result,
        });
        return;
      }
      final handler = routeHandlers[req.uri.path];
      if (handler != null) {
        final result = await handler();
        if (result == null) {
          await _respondJson(req, 200, {});
        } else {
          await _respondJson(req, 200, result);
        }
        return;
      }
      req.response.statusCode = 404;
      await req.response.close();
    });
  }

  Future<void> _respondJson(HttpRequest req, int status, Object body) async {
    req.response.statusCode = status;
    req.response.headers.contentType = ContentType.json;
    req.response.write(jsonEncode(body));
    await req.response.close();
  }

  Future<void> stop() => _server.close(force: true);
}

EscurelClient _client(_MockMcpServer mock) =>
    HttpEscurelClient(baseUrl: mock.baseUrl, dio: Dio());

void main() {
  late _MockMcpServer mock;
  late EscurelClient client;

  setUp(() async {
    mock = await _MockMcpServer.start();
    client = _client(mock);
  });

  tearDown(() async {
    client.close();
    await mock.stop();
  });

  group('MCP envelope', () {
    test('search posts JSON-RPC 2.0 to /mcp with method=tools/call and correct args', () async {
      Map<String, dynamic>? receivedArgs;
      mock.toolHandlers['search'] = (args) {
        receivedArgs = args;
        return {
          'hits': [
            {'page_id': 'contact__hoffmann', 'skill': 'contact', 'score': 0.92},
            {'page_id': 'engagement__hoffmann-intro', 'skill': 'engagement', 'score': 0.71},
          ],
        };
      };

      final r = await client.search(q: 'hoffmann', k: 5, skill: 'contact');

      expect(receivedArgs, {
        'q': 'hoffmann',
        'k': 5,
        'granularity': 'block',
        'page_type': 'any',
        'skill': 'contact',
      });
      expect(r.hits.map((h) => h.pageId).toList(),
          ['contact__hoffmann', 'engagement__hoffmann-intro']);
      expect(r.hits.first.score, closeTo(0.92, 1e-6));
    });

    test('resolve unmarshals page_type into the md.PageType enum', () async {
      mock.toolHandlers['resolve'] = (args) => {
            'page_id': args['wikilink'],
            'skill': 'opportunity',
            'page_type': 'instance',
            'exists': true,
            'description': 'pilot opportunity',
          };

      final r = await client.resolve('[[opportunity::hoffmann-pilot]]');
      expect(r.exists, isTrue);
      expect(r.pageType, md.PageType.instance);
      expect(r.description, 'pilot opportunity');
    });

    test('expand returns blocks + outgoing wikilinks shaped per spec', () async {
      mock.toolHandlers['expand'] = (args) => {
            'page_id': args['page_id'],
            'skill': 'opportunity',
            'page_type': 'instance',
            'frontmatter': {'value_eur': 60000, 'status': 'negotiating'},
            'body': '# Pilot\n\nMünchner Pharma',
            'blocks': [
              {'anchor': 'background', 'content': 'Hoffmann is champion.'},
            ],
            'wikilinks_out': ['[[customer::muenchner-pharma]]', '[[contact::hoffmann]]'],
            'version': 'v3',
          };

      final r = await client.expand('opportunity__hoffmann-pilot');
      expect(r.frontmatter['value_eur'], 60000);
      expect(r.body, contains('Pilot'));
      expect(r.blocks, hasLength(1));
      expect(r.blocks.single.anchor, 'background');
      expect(r.wikilinksOut, contains('[[customer::muenchner-pharma]]'));
      expect(r.version, 'v3');
    });

    test('neighbours returns Neighbour list shaped per spec', () async {
      mock.toolHandlers['neighbours'] = (args) => {
            'edges': [
              {'src': 'engagement__hoffmann-intro', 'dst': args['page_id'], 'link_skill': 'with'},
              {'src': 'lead__hoffmann-followup', 'dst': args['page_id'], 'link_skill': 'contact'},
            ],
          };
      final r = await client.neighbours('contact__hoffmann', direction: LinkDirection.incoming);
      expect(r, hasLength(2));
      expect(r.first.src, 'engagement__hoffmann-intro');
      expect(r.first.linkSkill, 'with');
    });

    test('neighbours maps LinkDirection to the gateway wire values in|out|both', () async {
      // The gateway's neighbours tool rejects `incoming`/`outgoing`; the
      // client must send `in`/`out`/`both`. A real socket captures it.
      String? sent;
      mock.toolHandlers['neighbours'] = (args) {
        sent = args['direction'] as String?;
        return {'edges': const []};
      };
      await client.neighbours('p', direction: LinkDirection.incoming);
      expect(sent, 'in');
      await client.neighbours('p', direction: LinkDirection.outgoing);
      expect(sent, 'out');
      await client.neighbours('p', direction: LinkDirection.both);
      expect(sent, 'both');
    });

    test('list_skills unmarshals required_frontmatter', () async {
      mock.toolHandlers['list_skills'] = (_) => {
            'skills': [
              {
                'id': 'engagement',
                'description': 'first-touch interaction',
                'required_frontmatter': ['at', 'with', 'channel'],
                'optional_frontmatter': ['outcome'],
              }
            ],
          };
      final r = await client.listSkills();
      expect(r, hasLength(1));
      expect(r.single.id, 'engagement');
      expect(r.single.requiredFrontmatter, ['at', 'with', 'channel']);
    });

    test('list_instances maps filter to frontmatter_key/value + order_by + limit', () async {
      Map<String, dynamic>? receivedArgs;
      mock.toolHandlers['list_instances'] = (args) {
        receivedArgs = args;
        return {'instances': []};
      };
      await client.listInstances(
        'lead',
        filter: const {'status': 'qualified'},
        orderBy: 'opened desc',
        limit: 10,
      );
      // The single-entry filter map becomes the server's
      // (frontmatter_key, frontmatter_value) pair (PR-5).
      expect(receivedArgs, {
        'skill_id': 'lead',
        'frontmatter_key': 'status',
        'frontmatter_value': 'qualified',
        'order_by': 'opened desc',
        'limit': 10,
      });
    });
  });

  group('errors', () {
    test('tool error envelope surfaces as EscurelToolException', () async {
      mock.nextError = (code: -32000, message: 'page not found');
      await expectLater(
        client.expand('not-a-real-page'),
        throwsA(isA<EscurelToolException>()
            .having((e) => e.message, 'message', contains('page not found'))),
      );
    });

    test('connection failure surfaces as EscurelTransportException', () async {
      final bad = HttpEscurelClient(baseUrl: 'http://127.0.0.1:1');
      await expectLater(
        bad.listSkills(),
        throwsA(isA<EscurelTransportException>()),
      );
      bad.close();
    });
  });

  group('substrate endpoints', () {
    test('healthz returns ok=true for HTTP 200', () async {
      mock.routeHandlers['/healthz'] = () => null;
      final h = await client.healthz();
      expect(h.ok, isTrue);
    });

    test('version maps server capability strings into BackendCapability set', () async {
      mock.routeHandlers['/version'] = () => {
            'app': 'escurel-server',
            'version': '0.3.0',
            'git_sha': 'abc1234',
            'capabilities': ['agentReadTools', 'agentWriteTools', 'unknownCapName'],
          };
      final v = await client.version();
      expect(v.version, '0.3.0');
      expect(v.capabilities, contains(BackendCapability.agentReadTools));
      expect(v.capabilities, contains(BackendCapability.agentWriteTools));
      // Unknown capability strings are dropped silently — forward compatibility.
      expect(v.capabilities.map((c) => c.name), isNot(contains('unknownCapName')));
    });
  });

  group('not-yet-implemented surfaces', () {
    // Write, chat, session, and the implemented admin ops tools now
    // round-trip over /mcp (covered by the no-mock integration tests
    // in escurel-server). The LaneStore inspection tools have no
    // server-side MCP implementation yet, so they remain the only
    // surfaces that throw EscurelUnsupportedException.
    test('lane inspection tools throw EscurelUnsupportedException', () async {
      await expectLater(
        client.adminListLanes(),
        throwsA(isA<EscurelUnsupportedException>()),
      );
      await expectLater(
        client.adminLaneKeys('fs'),
        throwsA(isA<EscurelUnsupportedException>()),
      );
      await expectLater(
        client.adminLaneBlob('fs', 'k'),
        throwsA(isA<EscurelUnsupportedException>()),
      );
    });
  });
}
