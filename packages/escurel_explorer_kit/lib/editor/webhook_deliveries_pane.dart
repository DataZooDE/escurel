/// RIGHT-panel tab — the **outbound webhook delivery log**: each
/// attempt the server made to call the tenant's configured webhook
/// sink (one per captured event), newest first. Backed by the admin
/// `admin_webhook_deliveries` tool (admin-gated; everything is admin
/// in dev/no-verifier mode). Shows "no webhook configured" when the
/// tenant has no sink set.
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../client/models.dart';
import '../state/providers.dart';
import '../theme/app_theme.dart';

class WebhookDeliveriesPane extends ConsumerStatefulWidget {
  const WebhookDeliveriesPane({super.key});

  @override
  ConsumerState<WebhookDeliveriesPane> createState() =>
      _WebhookDeliveriesPaneState();
}

class _WebhookDeliveriesPaneState extends ConsumerState<WebhookDeliveriesPane> {
  Future<WebhookDeliveries>? _future;

  @override
  void initState() {
    super.initState();
    _load();
  }

  void _load() {
    setState(() {
      _future = ref.read(escurelClientProvider).adminWebhookDeliveries();
    });
  }

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    return Semantics(
      label: 'webhook-deliveries-pane',
      container: true,
      explicitChildNodes: true,
      child: Container(
        key: const ValueKey('pane.webhook_deliveries'),
        color: kSurfaceContainerLow,
        padding: const EdgeInsets.all(12),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Expanded(
                  child: Text('Webhook deliveries', style: text.titleSmall),
                ),
                Semantics(
                  label: 'webhook-deliveries-refresh',
                  button: true,
                  onTap: _load,
                  excludeSemantics: true,
                  child: IconButton(
                    onPressed: _load,
                    iconSize: 18,
                    visualDensity: VisualDensity.compact,
                    tooltip: 'refresh',
                    icon: const Icon(Icons.refresh),
                  ),
                ),
              ],
            ),
            const SizedBox(height: 8),
            Expanded(
              child: FutureBuilder<WebhookDeliveries>(
                future: _future,
                builder: (context, snap) {
                  if (snap.connectionState == ConnectionState.waiting) {
                    return const Center(
                      child: CircularProgressIndicator(strokeWidth: 2),
                    );
                  }
                  if (snap.hasError) {
                    return Text(
                      '${snap.error}',
                      style: const TextStyle(color: kError),
                    );
                  }
                  final data = snap.data;
                  if (data == null || !data.configured) {
                    return Text(
                      'No webhook configured',
                      style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
                    );
                  }
                  if (data.deliveries.isEmpty) {
                    return Text(
                      'No deliveries yet',
                      style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
                    );
                  }
                  return ListView.separated(
                    key: const ValueKey('webhook_deliveries.list'),
                    itemCount: data.deliveries.length,
                    separatorBuilder: (_, _) => const SizedBox(height: 6),
                    itemBuilder: (context, i) =>
                        _DeliveryTile(delivery: data.deliveries[i]),
                  );
                },
              ),
            ),
          ],
        ),
      ),
    );
  }
}

class _DeliveryTile extends StatelessWidget {
  const _DeliveryTile({required this.delivery});

  final WebhookDelivery delivery;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    final ok = delivery.ok;
    return Semantics(
      label: 'webhook-delivery-item',
      container: true,
      explicitChildNodes: true,
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 6),
        decoration: BoxDecoration(
          color: kSurfaceContainerLowest,
          border: Border.all(color: kOutlineVariant),
          borderRadius: BorderRadius.circular(6),
        ),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Icon(
                  ok ? Icons.check_circle_outline : Icons.error_outline,
                  size: 14,
                  color: ok ? kSuccess : kError,
                ),
                const SizedBox(width: 6),
                if (delivery.httpStatus != null)
                  Text(
                    '${delivery.httpStatus}',
                    style: text.labelSmall?.copyWith(
                      color: ok ? kSuccess : kError,
                      fontWeight: FontWeight.w700,
                    ),
                  ),
                const Spacer(),
                Text(
                  _formatAt(delivery.atMs),
                  style: text.labelSmall?.copyWith(color: kOutline),
                ),
              ],
            ),
            const SizedBox(height: 4),
            Text(
              delivery.eventId,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: text.bodySmall?.copyWith(color: kOnSurface),
            ),
            if (delivery.error != null && delivery.error!.isNotEmpty) ...[
              const SizedBox(height: 4),
              Text(
                delivery.error!,
                style: text.labelSmall?.copyWith(color: kError),
              ),
            ],
          ],
        ),
      ),
    );
  }

  static String _formatAt(int atMs) {
    if (atMs <= 0) return '';
    final dt = DateTime.fromMillisecondsSinceEpoch(atMs, isUtc: true);
    final iso = dt.toIso8601String();
    final t = iso.indexOf('.');
    return t > 0 ? iso.substring(0, t) : iso;
  }
}
