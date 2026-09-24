// The host ↔ webview contract (SPEC §5): typed postMessage both ways.
// Shared by both tsconfigs, so nothing here may import `vscode` or Node.

export interface FieldView {
  name: string;
  label: string;
  /** `string | int | float | bool | date | datetime | enum | link` (or what the skill declared). */
  kind: string;
  /** `text | markdown | date | datetime | money | link | badge` — from `fields[].render`, else derived from `kind`. */
  render: string;
  required: boolean;
  value: unknown;
  /** The value as the form shows it. */
  display: string;
  values?: string[];
  link?: { skill: string; id: string; pageId: string };
}

export interface ActionView {
  skill: string;
  /** Until BACKEND_GAPS PR-2: "<Skill title> for <instance title> with an agent". */
  label: string;
}

export interface PageModel {
  pageId: string;
  title: string;
  skill: {
    id: string;
    description: string;
    summary?: string;
    autonomy: 'auto' | 'review' | 'confirm';
    layer: string;
    readOnly: boolean;
    backend: string;
  };
  fields: FieldView[];
  summary?: string;
  body: string;
  lastWrittenBy?: string | null;
  /** False until BACKEND_GAPS PR-1 (live personal drafts) lands. */
  editable: boolean;
  actions: ActionView[];
}

export type HostToWebview =
  { type: 'loading' } | { type: 'page'; model: PageModel } | { type: 'error'; message: string };

export type StartMode = 'background' | 'plan' | 'terminal';

export type WebviewToHost =
  | { type: 'ready' }
  | { type: 'open-page'; pageId: string }
  | { type: 'view-skill'; skill: string }
  | { type: 'show-raw' }
  | { type: 'refresh' }
  | { type: 'start-skill'; skill: string; mode: StartMode };
