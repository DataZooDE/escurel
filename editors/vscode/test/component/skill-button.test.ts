import { expect, fixture, html } from '@open-wc/testing';
import { splitButton } from '../../webview/shared/theme.css';
import { START_ITEMS, type EscurelSplitButton } from '../../webview/shared/skill-button';
import '../../webview/shared/skill-button';

// The menu is positioned by the page's stylesheet (the button renders into the light DOM).
const style = document.createElement('style');
style.textContent = splitButton.cssText;
document.head.append(style);

const raf = () => new Promise<void>((r) => requestAnimationFrame(() => r()));

describe('<escurel-split-button> menu placement', () => {
  it('opens upward when there is no room below, so every item can be reached', async () => {
    // A button at the very bottom of the viewport: a menu that always opens downward is clipped and
    // only its first item shows (seen in the live window, on a page scrolled to its end).
    const host = await fixture<HTMLElement>(
      html`<div style="position:fixed;left:16px;bottom:4px">
        <escurel-split-button label="Reassess risk" .items=${START_ITEMS}></escurel-split-button>
      </div>`,
    );
    const el = host.querySelector('escurel-split-button') as EscurelSplitButton;
    (el.querySelector('.chevron') as HTMLElement).click();
    await el.updateComplete;
    await raf();
    const menu = el.querySelector('[role="menu"]') as HTMLElement;
    const r = menu.getBoundingClientRect();
    expect(r.bottom <= window.innerHeight, `bottom ${r.bottom} of ${window.innerHeight}`).to.equal(
      true,
    );
    expect(r.top >= 0, `top ${r.top}`).to.equal(true);
  });

  it('opens downward when there is room, as menus normally do', async () => {
    const host = await fixture<HTMLElement>(
      html`<div style="position:fixed;left:16px;top:8px">
        <escurel-split-button label="Reassess risk" .items=${START_ITEMS}></escurel-split-button>
      </div>`,
    );
    const el = host.querySelector('escurel-split-button') as EscurelSplitButton;
    (el.querySelector('.chevron') as HTMLElement).click();
    await el.updateComplete;
    await raf();
    const menu = el.querySelector('[role="menu"]') as HTMLElement;
    const button = el.querySelector('.split') as HTMLElement;
    expect(menu.getBoundingClientRect().top >= button.getBoundingClientRect().bottom).to.equal(
      true,
    );
  });
});

// Relative luminance and contrast per WCAG 2.x.
function channel(v: number): number {
  const c = v / 255;
  return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
}
function luminance(rgb: string): number {
  const [r, g, b] = (rgb.match(/[\d.]+/g) ?? []).slice(0, 3).map(Number);
  return 0.2126 * channel(r!) + 0.7152 * channel(g!) + 0.0722 * channel(b!);
}
function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi! + 0.05) / (lo! + 0.05);
}

describe('<escurel-split-button> legibility', () => {
  // The Skill button is purple per the spec's colour roles. VS Code's dark and high-contrast themes
  // define charts.purple as a LIGHT purple, and white text on it was about 2:1.
  const themes: Record<string, string> = {
    light:
      '--escurel-skill:var(--vscode-charts-purple);--vscode-charts-purple:#652d90;--vscode-button-foreground:#ffffff',
    dark: '--escurel-skill:var(--vscode-charts-purple);--vscode-charts-purple:#b180d7;--vscode-button-foreground:#ffffff',
    'high contrast':
      '--escurel-skill:var(--vscode-charts-purple);--vscode-charts-purple:#b180d7;--vscode-button-foreground:#ffffff',
  };
  for (const [name, tokens] of Object.entries(themes)) {
    it(`keeps the label readable in ${name}`, async () => {
      const host = await fixture<HTMLElement>(
        html`<div style=${tokens}>
          <escurel-split-button
            class="skill-button"
            noun="skill"
            label="Reassess risk"
            .items=${START_ITEMS}
          ></escurel-split-button>
        </div>`,
      );
      const primary = host.querySelector('.primary') as HTMLElement;
      const cs = getComputedStyle(primary);
      const ratio = contrast(cs.backgroundColor, cs.color);
      expect(
        ratio >= 4.5,
        `${name}: ${cs.color} on ${cs.backgroundColor} = ${ratio.toFixed(2)}`,
      ).to.equal(true);
    });
  }
});
