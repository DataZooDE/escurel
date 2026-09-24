import * as vscode from 'vscode';
import { SCHEME } from '../fs/provider';
import { findWikilinks } from './wikilinks';

/** Every `[[skill::id]]` in an escurel: skill document is a link that resolves through the gateway (SPEC §3.8). */
export class WikilinkProvider implements vscode.DocumentLinkProvider {
  static register(context: vscode.ExtensionContext): void {
    context.subscriptions.push(
      vscode.languages.registerDocumentLinkProvider({ scheme: SCHEME }, new WikilinkProvider()),
    );
  }

  provideDocumentLinks(doc: vscode.TextDocument): vscode.DocumentLink[] {
    return findWikilinks(doc.getText()).map((l) => {
      const link = new vscode.DocumentLink(
        new vscode.Range(doc.positionAt(l.start), doc.positionAt(l.end)),
      );
      link.target = vscode.Uri.parse(
        `command:escurel.resolve?${encodeURIComponent(JSON.stringify([l.text]))}`,
      );
      link.tooltip = `Resolve ${l.skill}::${l.id}`;
      return link;
    });
  }
}
