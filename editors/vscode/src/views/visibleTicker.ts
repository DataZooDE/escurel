// A timer that runs only while a view is visible (no `vscode`): the Runtimes view re-reads on it, so
// a runner that dies goes from online to stale on screen without a manual refresh, and a hidden view
// costs nothing.

export class VisibleTicker {
  private timer: ReturnType<typeof setInterval> | undefined;

  constructor(
    private readonly periodMs: number,
    private readonly tick: () => void,
  ) {}

  /** Start ticking when the view shows, stop when it hides. */
  setVisible(visible: boolean): void {
    if (visible && !this.timer) this.timer = setInterval(this.tick, this.periodMs);
    else if (!visible) this.stop();
  }

  get running(): boolean {
    return this.timer !== undefined;
  }

  dispose(): void {
    this.stop();
  }

  private stop(): void {
    if (this.timer) clearInterval(this.timer);
    this.timer = undefined;
  }
}
