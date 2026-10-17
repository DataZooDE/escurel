import { describe, expect, it, vi } from 'vitest';
import {
  JSON_FILE_LIMIT,
  pickJsonObject,
  type JsonFileHost,
  type OpenJsonDoc,
} from '../../src/evolve/jsonObjectFile';

const enc = (text: string) => new TextEncoder().encode(text);
const opts = {
  what: 'instance file',
  placeHolder: 'pick',
  chooseDescription: 'd',
  openLabel: 'Choose',
};

function host(over: {
  doc?: Partial<OpenJsonDoc<string>> & { text?: string };
  bytes?: Uint8Array;
  choose?: 'doc' | 'file' | 'cancel';
  chosenFile?: string | undefined;
}) {
  const doc: OpenJsonDoc<string> = {
    uri: 'file:///a.json',
    name: 'a.json',
    fsPath: '/a.json',
    isDirty: false,
    getText: () => over.doc?.text ?? '{"pilot":"p0"}',
    ...over.doc,
  };
  const read = vi.fn(async () => over.bytes ?? enc(doc.getText()));
  const chooseFile = vi.fn(async () => over.chosenFile);
  const h: JsonFileHost<string> = {
    openJsonDocs: () => [doc],
    pick: async (choices) =>
      over.choose === 'cancel'
        ? undefined
        : over.choose === 'file'
          ? choices.find((c) => !c.doc)
          : choices.find((c) => c.doc),
    chooseFile,
    read,
  };
  return { h, read, chooseFile };
}

describe('picking the instance file', () => {
  it('reads a saved open editor', async () => {
    expect(await pickJsonObject(host({}).h, opts)).toEqual({ pilot: 'p0' });
  });

  it('refuses an editor with unsaved changes, even if the bytes on disk parse', async () => {
    await expect(pickJsonObject(host({ doc: { isDirty: true } }).h, opts)).rejects.toThrow(
      /unsaved changes/,
    );
    // clean flag but the editor text differs from disk: also stale
    await expect(pickJsonObject(host({ bytes: enc('{"pilot":"p5"}') }).h, opts)).rejects.toThrow(
      /unsaved changes/,
    );
  });

  it('refuses a file over 4 MB', async () => {
    const big = enc('{"a":"' + 'x'.repeat(JSON_FILE_LIMIT) + '"}');
    const h = host({ choose: 'file', chosenFile: 'file:///b.json', bytes: big });
    await expect(pickJsonObject(h.h, opts)).rejects.toThrow(/4 MB/);
  });

  it('refuses what is not one JSON object', async () => {
    for (const text of ['[1]', '"x"', 'null', '3'])
      await expect(
        pickJsonObject(host({ choose: 'file', chosenFile: 'f', bytes: enc(text) }).h, opts),
      ).rejects.toThrow(/one JSON object/);
    await expect(
      pickJsonObject(host({ choose: 'file', chosenFile: 'f', bytes: enc('{') }).h, opts),
    ).rejects.toThrow(/not valid JSON/);
  });

  it('returns undefined and reads nothing when the person cancels', async () => {
    let h = host({ choose: 'cancel' });
    expect(await pickJsonObject(h.h, opts)).toBeUndefined();
    expect(h.read).not.toHaveBeenCalled();
    h = host({ choose: 'file', chosenFile: undefined });
    expect(await pickJsonObject(h.h, opts)).toBeUndefined();
    expect(h.read).not.toHaveBeenCalled();
  });
});
