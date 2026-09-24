// Minimal `vscode` module stand-in for vitest: only what src/ imports at
// module load. Tests that need behaviour extend it explicitly.
export const workspace = {
  getConfiguration: () => ({ get: () => undefined }),
  onDidChangeConfiguration: () => ({ dispose() {} }),
};
export const window = {
  createOutputChannel: () => ({ info() {}, warn() {}, error() {}, debug() {}, trace() {} }),
};
export const commands = { registerCommand: () => ({ dispose() {} }) };
export class Disposable {
  static from() {
    return new Disposable();
  }
  dispose() {}
}
