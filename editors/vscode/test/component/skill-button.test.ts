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
