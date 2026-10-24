import * as vscode from 'vscode';
import { webviewHtml as shellHtml } from '../shared/webviewHtml';
import { THEME_CSS, THEME_TOKENS } from '../shared/themeTokens';
import { ReviewHookRegistry } from '../review/hooks';
import { PageAugmentRegistry } from './augment';
import { OverviewTileRegistry, PageRendererRegistry } from './contributions';
import { gatewayFacade } from './gateway';
import {
  EXTENSION_API_VERSION,
  type EscurelExtensionApi,
  type GatewayClient,
  type OverviewTileContribution,
  type PageAugment,
  type PageRenderer,
  type ReviewHook,
  type TenantInfo,
  type WebviewHtmlOptions,
} from './types';

const BUNDLE = /^[A-Za-z0-9_-]{1,64}$/;
const TAG = /^[a-z][a-z0-9]*(-[a-z0-9]+)+$/;

export interface ApiDeps {
  client: () => { callTool<T>(tool: string, args?: Record<string, unknown>): Promise<T> };
  gatewayUrl: () => string;
  /** Fires when the client was rebuilt (gateway or identity changed). */
  onDidChange: vscode.Event<void>;
}

/** What the host keeps beside the public object: the registries its own views read. */
export interface Contributions {
  tiles: OverviewTileRegistry;
  renderers: PageRendererRegistry;
  augments: PageAugmentRegistry;
  reviewHooks: ReviewHookRegistry;
  gateway: GatewayClient;
  onDidChangeTenant: vscode.Event<TenantInfo>;
}

export function createContributions(deps: ApiDeps): Contributions & { dispose(): void } {
  const changed = new vscode.EventEmitter<TenantInfo>();
  const sub = deps.onDidChange(() => changed.fire({ gatewayUrl: deps.gatewayUrl() }));
  return {
    tiles: new OverviewTileRegistry(),
    renderers: new PageRendererRegistry(),
    augments: new PageAugmentRegistry(),
    reviewHooks: new ReviewHookRegistry(),
    gateway: gatewayFacade(deps.client),
    onDidChangeTenant: changed.event,
    dispose: () => {
      sub.dispose();
      changed.dispose();
    },
  };
}

const sameOrigin = (a: string, b: string): boolean =>
  a.replace(/\/+$/, '') === b.replace(/\/+$/, '');

/**
 * The object `activate()` returns to other extensions (docs/design/extension-api.md). Frozen: a
 * companion cannot replace a member another companion relies on.
 */
export function createExtensionApi(deps: ApiDeps, c: Contributions): EscurelExtensionApi {
  return Object.freeze({
    apiVersion: EXTENSION_API_VERSION,
    getClient(tenant?: string): GatewayClient {
      if (tenant !== undefined && !sameOrigin(String(tenant), deps.gatewayUrl())) {
        throw new Error('getClient: this window is not connected to that tenant');
      }
      return c.gateway;
    },
    webviewHtml(o: WebviewHtmlOptions): string {
      if (typeof o?.bundle !== 'string' || !BUNDLE.test(o.bundle)) {
        throw new Error('webviewHtml: invalid bundle name');
      }
      if (typeof o.tag !== 'string' || !TAG.test(o.tag)) {
        throw new Error('webviewHtml: invalid element name');
      }
      return shellHtml(o.webview, o.extensionUri, o.bundle, o.tag, THEME_CSS);
    },
    themeTokens: THEME_TOKENS,
    themeCss: THEME_CSS,
    registerOverviewTile: (tile: OverviewTileContribution) => c.tiles.register(tile),
    registerPageRenderer: (skill: string, renderer: PageRenderer) =>
      c.renderers.register(skill, renderer),
    registerPageAugment: (skill: string, augment: PageAugment) =>
      c.augments.register(skill, augment),
    registerReviewHook: (hook: ReviewHook) => c.reviewHooks.register(hook),
    onDidChangeTenant: c.onDidChangeTenant,
  });
}
