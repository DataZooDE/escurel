import { LitElement, css, html, nothing } from 'lit';
import type { InspectorView } from '../../src/shared/protocol';
import { theme } from '../shared/theme.css';

/**
 * Placeholder, replaced by the inspector package. It exists so the canvas can compose
 * `<escurel-thread-inspector>` and be built and tested before that package lands: the
 * property below is the whole contract between the two.
 */
export class EscurelThreadInspector extends LitElement {
  static override properties = { detail: { attribute: false } };
  declare detail: InspectorView | undefined;

  static override styles = [theme, css``];

  protected override render() {
    return this.detail ? html`<h2>${this.detail.title}</h2>` : nothing;
  }
}
if (!customElements.get('escurel-thread-inspector')) {
  customElements.define('escurel-thread-inspector', EscurelThreadInspector);
}
