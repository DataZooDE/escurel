export interface VisibleLocalFile {
  uri: string;
  isDirty: boolean;
  text: string;
}

/** The reviewed disk bytes and an open editor must describe the same file. */
export function requireSavedVisibleFile(
  uri: string, savedText: string, openFiles: readonly VisibleLocalFile[],
): void {
  const visible = openFiles.find((file) => file.uri === uri);
  if (visible && (visible.isDirty || visible.text !== savedText))
    throw new Error('The selected file has unsaved or stale editor content. Save and review it before continuing.');
}
