/** A file the person's own editor has open, or one they choose: the host (VS Code) supplies these. */
export interface OpenJsonDoc<U> {
  uri: U;
  name: string;
  fsPath: string;
  isDirty: boolean;
  getText(): string;
}

export interface JsonFileHost<U> {
  openJsonDocs(): OpenJsonDoc<U>[];
  pick(
    choices: { label: string; description: string; doc: OpenJsonDoc<U> | undefined }[],
    placeHolder: string,
  ): Promise<{ doc: OpenJsonDoc<U> | undefined } | undefined>;
  chooseFile(openLabel: string): Promise<U | undefined>;
  read(uri: U): Promise<Uint8Array>;
}

export const JSON_FILE_LIMIT = 4_000_000;

/**
 * The person's own JSON object: an open editor (which must be saved, so the bytes read are the bytes
 * they see) or a local file they choose. Returns undefined when they cancel; throws, with a message
 * naming the problem, for an unsaved editor, a file over 4 MB, invalid JSON or anything but one object.
 */
export async function pickJsonObject<U>(
  host: JsonFileHost<U>,
  opts: { what: string; placeHolder: string; chooseDescription: string; openLabel: string },
): Promise<Record<string, unknown> | undefined> {
  const open = host.openJsonDocs().slice(0, 12);
  const choices = [
    ...open.map((doc) => ({
      label: `Use open file ${doc.name}`,
      description: doc.fsPath,
      doc,
    })),
    { label: 'Choose local JSON file', description: opts.chooseDescription, doc: undefined },
  ];
  const selected = await host.pick(choices, opts.placeHolder);
  if (!selected) return undefined;
  const doc = selected.doc;
  const file = doc ? doc.uri : await host.chooseFile(opts.openLabel);
  if (file === undefined) return undefined;
  const bytes = await host.read(file);
  if (doc && (doc.isDirty || new TextDecoder().decode(bytes) !== doc.getText()))
    throw new Error(
      `The ${opts.what} has unsaved changes in its editor. Save the file and choose it again.`,
    );
  if (bytes.length > JSON_FILE_LIMIT) throw new Error(`The ${opts.what} exceeds the 4 MB limit.`);
  let parsed: unknown;
  try {
    parsed = JSON.parse(new TextDecoder().decode(bytes));
  } catch {
    throw new Error(`The ${opts.what} is not valid JSON.`);
  }
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed))
    throw new Error(`The ${opts.what} must be one JSON object.`);
  return parsed as Record<string, unknown>;
}
