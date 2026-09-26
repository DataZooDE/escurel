// Minimal `vscode` module stand-in for vitest: only what src/ imports at
// module load. Tests that need behaviour extend it explicitly.
export const workspace = {
  getConfiguration: () => ({ get: () => undefined }),
  onDidChangeConfiguration: () => ({ dispose() {} }),
};
export const window = {
  createOutputChannel: () => ({ info() {}, warn() {}, error() {}, debug() {}, trace() {} }),
  showInformationMessage: () => Promise.resolve(undefined),
  showWarningMessage: () => Promise.resolve(undefined),
  showErrorMessage: () => Promise.resolve(undefined),
  showInputBox: () => Promise.resolve(undefined),
  showQuickPick: () => Promise.resolve(undefined),
};
export const commands = {
  registerCommand: () => ({ dispose() {} }),
  executeCommand: () => Promise.resolve(undefined),
};
export class Disposable {
  static from() {
    return new Disposable();
  }
  dispose() {}
}

export class Position {
  constructor(
    readonly line: number,
    readonly character: number,
  ) {}
}

export class Range {
  readonly start: Position;
  readonly end: Position;

  constructor(startLine: number, startCharacter: number, endLine: number, endCharacter: number) {
    this.start = new Position(startLine, startCharacter);
    this.end = new Position(endLine, endCharacter);
  }
}

export class Uri {
  readonly scheme: string;
  readonly authority: string;
  readonly path: string;
  readonly query: string;
  readonly fragment: string;

  constructor(scheme: string, authority: string, path: string, query: string, fragment: string) {
    this.scheme = scheme;
    this.authority = authority;
    this.path = path;
    this.query = query;
    this.fragment = fragment;
  }

  get fsPath(): string {
    return this.path;
  }

  with(change: {
    scheme?: string;
    authority?: string;
    path?: string;
    query?: string;
    fragment?: string;
  }): Uri {
    return new Uri(
      change.scheme ?? this.scheme,
      change.authority ?? this.authority,
      change.path ?? this.path,
      change.query ?? this.query,
      change.fragment ?? this.fragment,
    );
  }

  toString(): string {
    const auth = this.authority ? `//${this.authority}` : '';
    const q = this.query ? `?${this.query}` : '';
    const f = this.fragment ? `#${this.fragment}` : '';
    return `${this.scheme}:${auth}${this.path}${q}${f}`;
  }

  static file(path: string): Uri {
    return new Uri('file', '', path, '', '');
  }

  static parse(value: string): Uri {
    try {
      const u = new URL(value);
      return new Uri(
        u.protocol.replace(/:$/, ''),
        u.host,
        u.pathname,
        u.search.replace(/^\?/, ''),
        u.hash.replace(/^#/, ''),
      );
    } catch {
      const idx = value.indexOf(':');
      const scheme = idx >= 0 ? value.slice(0, idx) : '';
      const rest = idx >= 0 ? value.slice(idx + 1) : value;
      return new Uri(scheme, '', rest, '', '');
    }
  }

  static from(components: {
    scheme: string;
    authority?: string;
    path?: string;
    query?: string;
    fragment?: string;
  }): Uri {
    return new Uri(
      components.scheme,
      components.authority ?? '',
      components.path ?? '',
      components.query ?? '',
      components.fragment ?? '',
    );
  }
}

export class ThemeIcon {
  constructor(
    readonly id: string,
    readonly color?: ThemeColor,
  ) {}
}

export class ThemeColor {
  constructor(readonly id: string) {}
}

export class EventEmitter<T> {
  private readonly listeners: ((e: T) => void)[] = [];

  readonly event = (listener: (e: T) => void): Disposable => {
    this.listeners.push(listener);
    return new Disposable();
  };

  fire(data: T): void {
    for (const l of this.listeners) l(data);
  }
}

export enum CommentMode {
  Preview = 0,
  Editing = 1,
}

export enum CommentThreadCollapsibleState {
  Collapsed = 0,
  Expanded = 1,
}
