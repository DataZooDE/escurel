import * as vscode from 'vscode';
import { REVIEW_SCHEME, parseReviewParts, reviewPath, type ReviewSide } from './reviewModel';

/**
 * The two sides of a draft diff are served from `escurel-review:` URIs
 * rather than temp files, so nothing of a draft ever touches disk.
 */
export function encodeReviewUri(draftId: string, side: ReviewSide): vscode.Uri {
  return vscode.Uri.from({ scheme: REVIEW_SCHEME, path: reviewPath(draftId, side) });
}

export function decodeReviewUri(
  uri: vscode.Uri,
): { draftId: string; side: ReviewSide } | undefined {
  return parseReviewParts(uri.scheme, uri.path, uri.authority);
}
