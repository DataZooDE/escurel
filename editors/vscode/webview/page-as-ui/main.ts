import { LitElement, html } from 'lit';
import { theme } from '../shared/theme.css';

/** Placeholder until the page-as-UI component lands (M1 step 7). */
export class EscurelPageAsUi extends LitElement {
  static styles = [theme];
  render() {
    return html`<p>escurel page-as-UI</p>`;
  }
}
customElements.define('escurel-page-as-ui', EscurelPageAsUi);
