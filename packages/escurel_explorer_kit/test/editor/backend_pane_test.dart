// Widget tests for the external-backend section of an instance page. The
// BackendPane is provider-free — it renders straight from an ExpandResult —
// so each case pumps a hand-built page and asserts the stable semantics
// labels (the rodney selector contract) and the read-only framing.

import 'package:escurel_explorer_kit/client/models.dart';
import 'package:escurel_explorer_kit/editor/backend_pane.dart';
import 'package:escurel_explorer_kit/md/frontmatter.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

ExpandResult _page({
  required Map<String, dynamic> frontmatter,
  List<Block> blocks = const [],
  BackendProjection? projection,
  int? chunksTotal,
  bool chunksTruncated = false,
}) => ExpandResult(
  pageId: 'sql_view/skills/erp_customer/instances/c-1',
  skill: 'erp_customer',
  pageType: PageType.instance,
  frontmatter: frontmatter,
  body: '',
  blocks: blocks,
  wikilinksOut: const [],
  backendProjection: projection,
  chunksTotal: chunksTotal,
  chunksTruncated: chunksTruncated,
);

Future<void> _pump(WidgetTester tester, ExpandResult page) async {
  tester.view.physicalSize = const Size(1400, 900);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(
    MaterialApp(
      home: Scaffold(
        body: SingleChildScrollView(child: BackendPane(page: page)),
      ),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('markdown instance renders nothing', (tester) async {
    await _pump(tester, _page(frontmatter: const {'name': 'Acme'}));
    expect(find.byType(BackendPane), findsOneWidget);
    expect(find.textContaining('read-only'), findsNothing);
    expect(find.byKey(const ValueKey('entity_editor.backend')), findsNothing);
  });

  testWidgets('sql_view instance renders the bounded projection', (
    tester,
  ) async {
    final page = _page(
      frontmatter: const {
        'name': 'Acme',
        'backend_ref': {'kind': 'sql_view', 'view': 'vw_erp_customer_c1'},
      },
      projection: const BackendProjection(
        view: 'vw_erp_customer_c1',
        rows: [
          {'id': 1, 'name': 'Acme', 'tier': 'gold'},
          {'id': 2, 'name': 'Globex', 'tier': 'silver'},
        ],
        source: {'name': 'ACME CORP'},
        truncated: true,
      ),
    );
    await _pump(tester, page);

    expect(
      find.bySemanticsLabel('backend-pane:${page.pageId}'),
      findsOneWidget,
    );
    expect(find.bySemanticsLabel('backend-projection'), findsOneWidget);
    expect(find.text('SQL view (read-only)'), findsOneWidget);
    // Row data renders.
    expect(find.text('Globex'), findsOneWidget);
    expect(find.text('gold'), findsOneWidget);
    // Truncation is surfaced.
    expect(find.textContaining('bounded projection'), findsOneWidget);
    // No degraded banner on a healthy binding.
    expect(find.bySemanticsLabel('binding-degraded'), findsNothing);
  });

  testWidgets('a degraded sql_view binding surfaces the fail-closed banner', (
    tester,
  ) async {
    final page = _page(
      frontmatter: const {
        'backend_ref': {'kind': 'sql_view', 'view': 'vw_erp_customer_c1'},
      },
      projection: const BackendProjection(
        view: 'vw_erp_customer_c1',
        rows: [],
        source: {},
        issueCode: 'binding_degraded',
        issueMessage: 'source column `tier` disappeared',
      ),
    );
    await _pump(tester, page);

    expect(find.bySemanticsLabel('binding-degraded'), findsOneWidget);
    expect(find.text('binding_degraded'), findsOneWidget);
    expect(find.textContaining('fail closed'), findsOneWidget);
  });

  testWidgets('document instance surfaces chunk count + truncation', (
    tester,
  ) async {
    const page = ExpandResult(
      pageId: 'document/skills/contract/instances/d-1',
      skill: 'contract',
      pageType: PageType.instance,
      frontmatter: {
        'backend_ref': {
          'kind': 'document',
          'blob_id': 'sha256:abcd',
          'status': 'materialised',
          'extract_engine': 'kreuzberg',
        },
      },
      body: '',
      blocks: [
        Block(anchor: 'c0', content: 'lead chunk one'),
        Block(anchor: 'c1', content: 'lead chunk two'),
      ],
      wikilinksOut: [],
      chunksTotal: 42,
      chunksTruncated: true,
    );
    await _pump(tester, page);

    expect(
      find.bySemanticsLabel('backend-pane:${page.pageId}'),
      findsOneWidget,
    );
    expect(find.bySemanticsLabel('document-chunks'), findsOneWidget);
    expect(find.text('Document (read-only)'), findsOneWidget);
    expect(find.text('materialised'), findsOneWidget);
    expect(find.textContaining('via kreuzberg'), findsOneWidget);
    expect(find.text('showing 2 of 42 chunks'), findsOneWidget);
  });

  testWidgets('a remote openapi instance renders the live projection', (
    tester,
  ) async {
    // The remote wire shape differs from sql_view: `source` is the endpoint
    // NAME (a string) and the projected values arrive under `fields`.
    final page = _page(
      frontmatter: const {
        'name': 'AAPL',
        'backend_ref': {'kind': 'openapi', 'endpoint': 'yahoo_finance'},
      },
      projection: const BackendProjection(
        endpoint: 'yahoo_finance',
        fields: {'symbol': 'AAPL', 'price': 189.31, 'currency': 'USD'},
      ),
    );
    await _pump(tester, page);

    expect(
      find.bySemanticsLabel('backend-pane:${page.pageId}'),
      findsOneWidget,
    );
    expect(find.bySemanticsLabel('backend-projection'), findsOneWidget);
    expect(find.text('Remote openapi (live)'), findsOneWidget);
    // The projected fields render as key/value rows.
    expect(find.text('symbol'), findsOneWidget);
    expect(find.text('AAPL'), findsOneWidget);
    expect(find.text('price'), findsOneWidget);
    expect(find.text('189.31'), findsOneWidget);
    expect(find.text('currency'), findsOneWidget);
    expect(find.text('USD'), findsOneWidget);
    // Provenance: the endpoint the values were fetched from.
    expect(find.textContaining('yahoo_finance'), findsWidgets);
    expect(find.bySemanticsLabel('binding-degraded'), findsNothing);
  });

  testWidgets('a degraded remote read surfaces the fail-closed issue', (
    tester,
  ) async {
    // A remote failure arrives as `{ issue: "<message>" }` — a plain string,
    // not the sql_view `{code, message}` object. The message must render
    // verbatim and prominently: the fail-closed path is part of the demo.
    final page = _page(
      frontmatter: const {
        'backend_ref': {'kind': 'mcp', 'endpoint': 'upstream_kb'},
      },
      projection: const BackendProjection(
        issueMessage: 'endpoint `upstream_kb` is not registered',
      ),
    );
    await _pump(tester, page);

    expect(find.text('Remote mcp (live)'), findsOneWidget);
    expect(find.bySemanticsLabel('binding-degraded'), findsOneWidget);
    expect(
      find.text('endpoint `upstream_kb` is not registered'),
      findsOneWidget,
    );
    expect(find.textContaining('fail closed'), findsOneWidget);
    // No fields table pretends data came back.
    expect(find.bySemanticsLabel('backend-projection'), findsNothing);
  });

  testWidgets('a remote projection with no fields states so honestly', (
    tester,
  ) async {
    final page = _page(
      frontmatter: const {
        'backend_ref': {'kind': 'openapi', 'endpoint': 'crm_rest'},
      },
      projection: const BackendProjection(endpoint: 'crm_rest'),
    );
    await _pump(tester, page);
    expect(find.textContaining('no projected fields'), findsOneWidget);
    expect(find.bySemanticsLabel('binding-degraded'), findsNothing);
  });

  testWidgets('the server `ok` document status renders as healthy', (
    tester,
  ) async {
    // The server stamps a healthy document `status: ok` (not `materialised`).
    const page = ExpandResult(
      pageId: 'document/skills/contract/instances/d-2',
      skill: 'contract',
      pageType: PageType.instance,
      frontmatter: {
        'backend_ref': {'kind': 'document', 'status': 'ok'},
      },
      body: '',
      blocks: [Block(anchor: 'c0', content: 'only chunk')],
      wikilinksOut: [],
      chunksTotal: 1,
    );
    await _pump(tester, page);
    expect(find.bySemanticsLabel('document-chunks'), findsOneWidget);
    expect(find.text('ok'), findsOneWidget);
    expect(find.text('1 chunks'), findsOneWidget);
  });
}
