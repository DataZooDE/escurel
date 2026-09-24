import * as vscode from 'vscode';
import { EscurelError, type EscurelClient } from '../client';
import { log } from '../log';
import { pageIdFromPath, pathForPage, readPageMarkdown } from './read';
import { writeSkill } from './write';

export const SCHEME = 'escurel';

export function uriForPage(pageId: string): vscode.Uri {
  return vscode.Uri.from({ scheme: SCHEME, path: pathForPage(pageId) });
}

/**
 * `escurel:` — skills read-write (save = `update_page` with the read-time
 * `content_sha256`), instances read-only (they are edited in page-as-UI).
 * Directory listings come from `list_skills` / `list_instances` so the
 * Explorer-style views can walk the tree; nothing is cached across reads
 * (SPEC §1: online only).
 */
export class EscurelFileSystem implements vscode.FileSystemProvider {
  private readonly emitter = new vscode.EventEmitter<vscode.FileChangeEvent[]>();
  readonly onDidChangeFile = this.emitter.event;
  /** The hash each open page was last read at, keyed by path — the CAS base for its next save. */
  private readonly baseSha = new Map<string, string | undefined>();
  private readonly degraded = new Set<string>();

  constructor(private readonly client: () => EscurelClient) {}

  static register(
    context: vscode.ExtensionContext,
    client: () => EscurelClient,
  ): EscurelFileSystem {
    const fs = new EscurelFileSystem(client);
    context.subscriptions.push(
      vscode.workspace.registerFileSystemProvider(SCHEME, fs, { isCaseSensitive: true }),
    );
    return fs;
  }

  watch(): vscode.Disposable {
    return new vscode.Disposable(() => undefined);
  }

  async stat(uri: vscode.Uri): Promise<vscode.FileStat> {
    const p = pageIdFromPath(uri.path);
    if (!p) throw vscode.FileSystemError.FileNotFound(uri);
    if (!p.pageId) return { type: vscode.FileType.Directory, ctime: 0, mtime: 0, size: 0 };
    const page = await this.read(uri, p.pageId);
    const readonly = p.kind === 'instance';
    return {
      type: vscode.FileType.File,
      ctime: 0,
      mtime: Date.now(),
      size: Buffer.byteLength(page.text),
      permissions: readonly ? vscode.FilePermission.Readonly : undefined,
    };
  }

  async readDirectory(uri: vscode.Uri): Promise<[string, vscode.FileType][]> {
    const p = pageIdFromPath(uri.path);
    if (!p || p.pageId) throw vscode.FileSystemError.FileNotADirectory(uri);
    switch (p.kind) {
      case 'root':
        return [
          ['skills', vscode.FileType.Directory],
          ['instances', vscode.FileType.Directory],
        ];
      case 'skills-root':
        return (await this.client().listSkills()).map((s) => [`${s.id}.md`, vscode.FileType.File]);
      case 'instances-root':
        return (await this.client().listSkills()).map((s) => [s.id, vscode.FileType.Directory]);
      case 'instances-skill': {
        const out: [string, vscode.FileType][] = [];
        for await (const page of this.client().listInstances({ skill_id: p.skill!, limit: 500 })) {
          for (const i of page.instances)
            out.push([i.page_id.split('/').pop()!, vscode.FileType.File]);
        }
        return out;
      }
      default:
        throw vscode.FileSystemError.FileNotADirectory(uri);
    }
  }

  async readFile(uri: vscode.Uri): Promise<Uint8Array> {
    const p = pageIdFromPath(uri.path);
    if (!p?.pageId) throw vscode.FileSystemError.FileNotFound(uri);
    const page = await this.read(uri, p.pageId);
    return Buffer.from(page.text, 'utf8');
  }

  private async read(uri: vscode.Uri, pageId: string) {
    let page;
    try {
      page = await readPageMarkdown(this.client(), pageId);
    } catch (e) {
      throw this.mapError(uri, e);
    }
    if (!page) throw vscode.FileSystemError.FileNotFound(uri);
    this.baseSha.set(uri.path, page.sha256);
    if (page.degraded) {
      if (!this.degraded.has(uri.path))
        log().warn(
          `escurel: ${pageId} was reassembled from frontmatter + body (the gateway sent no raw content); a save may change its formatting`,
        );
      this.degraded.add(uri.path);
    } else this.degraded.delete(uri.path);
    return page;
  }

  async writeFile(uri: vscode.Uri, content: Uint8Array): Promise<void> {
    const p = pageIdFromPath(uri.path);
    if (!p?.pageId) throw vscode.FileSystemError.FileNotFound(uri);
    if (p.kind !== 'skill')
      throw vscode.FileSystemError.NoPermissions(
        'escurel: instances are edited in the page view, not as raw markdown',
      );
    const text = Buffer.from(content).toString('utf8');
    try {
      await writeSkill(this.client(), p.pageId, text, this.baseSha.get(uri.path));
    } catch (e) {
      throw this.mapError(uri, e);
    }
    // Re-read the hash for the next save (the gateway is the authority on what it stored).
    const fresh = await readPageMarkdown(this.client(), p.pageId).catch(() => undefined);
    this.baseSha.set(uri.path, fresh?.sha256);
    this.emitter.fire([{ type: vscode.FileChangeType.Changed, uri }]);
  }

  /** The head, when the last save was refused as a conflict, so the caller can offer a diff. */
  private mapError(uri: vscode.Uri, e: unknown): Error {
    if (e instanceof EscurelError) {
      switch (e.kind) {
        case 'not_found':
          return vscode.FileSystemError.FileNotFound(uri);
        case 'forbidden':
        case 'layer_read_only':
        case 'admin_required':
        case 'unauthorized':
          return vscode.FileSystemError.NoPermissions(`escurel: ${e.message}`);
        case 'conflict':
          return vscode.FileSystemError.Unavailable(
            `escurel: the page changed on the gateway since you opened it (conflict). Reload it and re-apply your edit.`,
          );
        case 'refused':
          return vscode.FileSystemError.Unavailable(
            `escurel: refused — ${e.issues?.map((i) => `${i.code}: ${i.message}`).join('; ') ?? e.message}`,
          );
        default:
          return vscode.FileSystemError.Unavailable(`escurel: ${e.kind}: ${e.message}`);
      }
    }
    return e instanceof Error ? e : new Error(String(e));
  }

  delete(): void {
    throw vscode.FileSystemError.NoPermissions('escurel: pages are not deleted from the editor');
  }
  rename(): void {
    throw vscode.FileSystemError.NoPermissions('escurel: pages are not renamed from the editor');
  }
  createDirectory(): void {
    throw vscode.FileSystemError.NoPermissions("escurel: the tree is the gateway's");
  }
}
