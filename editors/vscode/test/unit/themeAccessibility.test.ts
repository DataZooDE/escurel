import { describe, expect, it } from 'vitest';
import { theme } from '../../webview/shared/theme.css';

const css = theme.cssText;

// No webview had a rule for the two OS-level accessibility modes: nothing stopped motion for people
// who ask for less of it, and nothing kept chips and buttons visible when the system forces colours.
describe('the shared webview theme', () => {
  it('stops animation and smooth scrolling when the person asks for reduced motion', () => {
    expect(css).toContain('@media (prefers-reduced-motion: reduce)');
    const block = css.slice(css.indexOf('@media (prefers-reduced-motion: reduce)'));
    expect(block).toMatch(/animation:\s*none/);
    expect(block).toMatch(/transition:\s*none/);
    expect(block).toMatch(/scroll-behavior:\s*auto/);
  });

  it('keeps chips, badges and buttons outlined when the system forces colours', () => {
    expect(css).toContain('@media (forced-colors: active)');
    const block = css.slice(css.indexOf('@media (forced-colors: active)'));
    expect(block).toMatch(/\.chip/);
    expect(block).toMatch(/border:\s*1px solid (ButtonText|CanvasText)/);
  });
});
