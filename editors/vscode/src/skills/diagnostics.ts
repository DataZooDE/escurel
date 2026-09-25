import * as vscode from 'vscode';
import type { EscurelClient } from '../client';
import { log } from '../log';
import { pageIdFromPath } from '../fs/read';
import { SCHEME } from '../fs/provider';

/**
 * `validate` on every change of an `escurel:` skill document (debounced),
 * mapped to Diagnostics: severity, the issue's `location` as the range (a
 * frontmatter key when it names one, else the first line), and the
 * suggestion appended to the message (SPEC §1).
 */
export function registerSkillDiagnostics(
  context: vscode.ExtensionContext,
  client: () => EscurelClient,
): void {
  const diagnostics = vscode.languages.createDiagnosticCollection('escurel');
  const timers = new Map<string, NodeJS.Timeout>();

  const run = async (doc: vscode.TextDocument) => {
    const p = pageIdFromPath(doc.uri.path);
    if (doc.uri.scheme !== SCHEME || p?.kind !== 'skill') return;
    try {
      const v = await client().validate({ content: doc.getText(), as_page_id: p.pageId });
      diagnostics.set(
        doc.uri,
        v.issues.map((i) => toDiagnostic(doc, i)),
      );
    } catch (e) {
      log().warn(`escurel: validate failed for ${p.pageId}: ${(e as Error).message}`);
    }
  };
  const schedule = (doc: vscode.TextDocument) => {
    const key = doc.uri.toString();
    clearTimeout(timers.get(key));
    timers.set(
      key,
      setTimeout(() => void run(doc), 400),
    );
  };

  context.subscriptions.push(
    diagnostics,
    vscode.workspace.onDidOpenTextDocument(schedule),
    vscode.workspace.onDidChangeTextDocument((e) => schedule(e.document)),
    vscode.workspace.onDidCloseTextDocument((doc) => {
      clearTimeout(timers.get(doc.uri.toString()));
      diagnostics.delete(doc.uri);
    }),
  );
  for (const doc of vscode.workspace.textDocuments) schedule(doc);
}

export function toDiagnostic(
  doc: vscode.TextDocument,
  issue: { severity: string; code: string; location: string; message: string; suggestion?: string },
): vscode.Diagnostic {
  const range = locate(doc, issue.location);
  const message = issue.suggestion ? `${issue.message} — ${issue.suggestion}` : issue.message;
  const d = new vscode.Diagnostic(
    range,
    message,
    issue.severity === 'error'
      ? vscode.DiagnosticSeverity.Error
      : vscode.DiagnosticSeverity.Warning,
  );
  d.code = issue.code;
  d.source = 'escurel';
  return d;
}

/** `frontmatter.<key>[…]` → the line declaring `<key>:` inside the frontmatter block; anything else → line 0. */
function locate(doc: vscode.TextDocument, location: string): vscode.Range {
  const key = /^frontmatter\.([A-Za-z0-9_-]+)/.exec(location)?.[1];
  if (key) {
    const lines = doc.getText().split('\n');
    const end = lines.indexOf('---', 1);
    for (let i = 1; i < (end === -1 ? lines.length : end); i++) {
      if (lines[i]!.startsWith(`${key}:`)) return new vscode.Range(i, 0, i, lines[i]!.length);
    }
  }
  return new vscode.Range(0, 0, 0, doc.lineAt(0).text.length);
}
