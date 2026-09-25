import { LitElement, html, nothing } from 'lit';
import { property, state } from 'lit/decorators.js';

export interface MenuItem {
  id: string;
  label: string;
}

/**
 * The Skill / Instance split button (SPEC §2): a primary segment that acts
 * at once, and a chevron that opens a menu. The menu is a `role="menu"`
 * with roving focus, Arrow/Home/End, Escape and outside-click to close;
 * focus returns to the chevron. Emits `primary` and `select` (item id).
 */
export class EscurelSplitButton extends LitElement {
  static properties = {
    noun: { type: String },
    label: { type: String },
    title: { type: String },
    header: { type: String },
    items: { attribute: false },
    open: { state: true },
  };

  @property() noun: 'skill' | 'instance' = 'skill';
  @property() label = '';
  @property() override title = '';
  @property() header = '';
  @property({ attribute: false }) items: MenuItem[] = [];
  @state() private open = false;

  /** Light DOM: the page's one stylesheet (theme + splitButton) styles it and tests can reach it. */
  protected override createRenderRoot() {
    return this;
  }
  private readonly onDocClick = (e: MouseEvent) => {
    if (this.open && !e.composedPath().includes(this)) this.close();
  };

  override connectedCallback(): void {
    super.connectedCallback();
    document.addEventListener('mousedown', this.onDocClick);
  }
  override disconnectedCallback(): void {
    document.removeEventListener('mousedown', this.onDocClick);
    super.disconnectedCallback();
  }

  private toggle(): void {
    if (this.open) this.close();
    else this.openMenu();
  }
  private openMenu(): void {
    this.open = true;
    void this.updateComplete.then(() =>
      (this.renderRoot.querySelector('[role="menuitem"]') as HTMLElement | null)?.focus(),
    );
  }
  private close(): void {
    if (!this.open) return;
    this.open = false;
    (this.renderRoot.querySelector('.chevron') as HTMLElement | null)?.focus();
    this.dispatchEvent(new CustomEvent('escurel-menu-closed', { bubbles: true, composed: true }));
  }
  private select(id: string): void {
    this.dispatchEvent(new CustomEvent('select', { detail: id, bubbles: true, composed: true }));
    this.close();
  }
  private onMenuKey(e: KeyboardEvent): void {
    const items = Array.from(this.renderRoot.querySelectorAll<HTMLElement>('[role="menuitem"]'));
    const i = items.indexOf(e.target as HTMLElement);
    const go = (n: number) => {
      e.preventDefault();
      items[(n + items.length) % items.length]?.focus();
    };
    switch (e.key) {
      case 'Escape':
        e.preventDefault();
        return this.close();
      case 'ArrowDown':
        return go(i + 1);
      case 'ArrowUp':
        return go(i - 1);
      case 'Home':
        return go(0);
      case 'End':
        return go(items.length - 1);
      case 'Tab':
        return this.close();
    }
  }

  override render() {
    return html`<span class="split">
      <button
        class="primary"
        title=${this.title}
        @click=${() => this.dispatchEvent(new CustomEvent('primary', { bubbles: true, composed: true }))}
      >
        ${this.label}
      </button>
      <button
        class="chevron"
        aria-haspopup="menu"
        aria-expanded=${this.open ? 'true' : 'false'}
        aria-label="More actions"
        @click=${this.toggle}
      >
        ▾
      </button>
      ${
        this.open
          ? html`<ul role="menu" @keydown=${this.onMenuKey}>
              ${this.header ? html`<li class="menu-header">${this.header}</li>` : nothing}
              ${this.items.map((it) => html`<li><button role="menuitem" tabindex="-1" @click=${() => this.select(it.id)}>${it.label}</button></li>`)}
            </ul>`
          : nothing
      }
    </span>`;
  }
}
customElements.define('escurel-split-button', EscurelSplitButton);
