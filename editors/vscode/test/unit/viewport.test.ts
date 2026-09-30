import { describe, expect, it } from 'vitest';
import {
  clampZoom,
  fitToBounds,
  panToReveal,
  zoomAboutPoint,
  MAX_ZOOM,
  MIN_ZOOM,
} from '../../webview/thread/viewport';

describe('viewport pure arithmetic', () => {
  describe('clampZoom', () => {
    it('clamps zoom below 0.4 to 0.4', () => {
      expect(clampZoom(0.1)).to.equal(MIN_ZOOM);
      expect(clampZoom(0.399)).to.equal(MIN_ZOOM);
    });

    it('clamps zoom above 2.5 to 2.5', () => {
      expect(clampZoom(3.0)).to.equal(MAX_ZOOM);
      expect(clampZoom(2.501)).to.equal(MAX_ZOOM);
    });

    it('preserves zoom within [0.4, 2.5]', () => {
      expect(clampZoom(1.0)).to.equal(1.0);
      expect(clampZoom(0.4)).to.equal(0.4);
      expect(clampZoom(2.5)).to.equal(2.5);
      expect(clampZoom(1.75)).to.equal(1.75);
    });
  });

  describe('zoomAboutPoint', () => {
    it('leaves the cursor point fixed in container coordinates when zooming in', () => {
      const current = { x: 50, y: 30, zoom: 1.0 };
      const cursor = { x: 200, y: 150 };
      const nextZoom = 1.5;

      const next = zoomAboutPoint(current, cursor, nextZoom);

      // World point before zoom:
      const worldX = (cursor.x - current.x) / current.zoom;
      const worldY = (cursor.y - current.y) / current.zoom;

      // Container point after zoom:
      const containerXAfter = worldX * next.zoom + next.x;
      const containerYAfter = worldY * next.zoom + next.y;

      expect(containerXAfter).to.be.closeTo(cursor.x, 0.0001);
      expect(containerYAfter).to.be.closeTo(cursor.y, 0.0001);
      expect(next.zoom).to.equal(1.5);
    });

    it('leaves the cursor point fixed in container coordinates when zooming out', () => {
      const current = { x: -100, y: -50, zoom: 2.0 };
      const cursor = { x: 300, y: 400 };
      const nextZoom = 1.0;

      const next = zoomAboutPoint(current, cursor, nextZoom);

      const worldX = (cursor.x - current.x) / current.zoom;
      const worldY = (cursor.y - current.y) / current.zoom;

      const containerXAfter = worldX * next.zoom + next.x;
      const containerYAfter = worldY * next.zoom + next.y;

      expect(containerXAfter).to.be.closeTo(cursor.x, 0.0001);
      expect(containerYAfter).to.be.closeTo(cursor.y, 0.0001);
      expect(next.zoom).to.equal(1.0);
    });

    it('clamps zoom to [0.4, 2.5] while preserving point invariance', () => {
      const current = { x: 0, y: 0, zoom: 1.0 };
      const cursor = { x: 100, y: 100 };

      const clampedLow = zoomAboutPoint(current, cursor, 0.1);
      expect(clampedLow.zoom).to.equal(MIN_ZOOM);

      const worldXLow = (cursor.x - current.x) / current.zoom;
      const containerXLow = worldXLow * clampedLow.zoom + clampedLow.x;
      expect(containerXLow).to.be.closeTo(cursor.x, 0.0001);

      const clampedHigh = zoomAboutPoint(current, cursor, 5.0);
      expect(clampedHigh.zoom).to.equal(MAX_ZOOM);

      const worldXHigh = (cursor.x - current.x) / current.zoom;
      const containerXHigh = worldXHigh * clampedHigh.zoom + clampedHigh.x;
      expect(containerXHigh).to.be.closeTo(cursor.x, 0.0001);
    });
  });

  describe('fitToBounds', () => {
    it('centres and scales to bounds when graph is wider than container', () => {
      const bounds = { width: 1000, height: 400 };
      const container = { width: 500, height: 400 };

      const fitted = fitToBounds(bounds, container);

      // Width ratio is 500 / 1000 = 0.5. Height ratio is 400 / 400 = 1.0.
      // Fits by width: zoom = 0.5.
      expect(fitted.zoom).to.equal(0.5);
      // Scaled bounds: width = 500, height = 200.
      // Horizontally fills container: x = 0.
      expect(fitted.x).to.be.closeTo(0, 0.0001);
      // Vertically centered: (400 - 200) / 2 = 100.
      expect(fitted.y).to.be.closeTo(100, 0.0001);
    });

    it('centres and scales to bounds when graph is taller than container', () => {
      const bounds = { width: 400, height: 800 };
      const container = { width: 600, height: 400 };

      const fitted = fitToBounds(bounds, container);

      // Width ratio = 600 / 400 = 1.5. Height ratio = 400 / 800 = 0.5.
      // Fits by height: zoom = 0.5.
      expect(fitted.zoom).to.equal(0.5);
      // Scaled bounds: width = 200, height = 400.
      // Horizontally centered: (600 - 200) / 2 = 200.
      expect(fitted.x).to.be.closeTo(200, 0.0001);
      // Vertically fills container: y = 0.
      expect(fitted.y).to.be.closeTo(0, 0.0001);
    });

    it('clamps fit zoom within [0.4, 2.5]', () => {
      // Extremely large graph that would require zoom 0.1
      const hugeBounds = { width: 10000, height: 10000 };
      const container = { width: 1000, height: 1000 };
      const fittedLow = fitToBounds(hugeBounds, container);
      expect(fittedLow.zoom).to.equal(MIN_ZOOM);

      // Tiny graph that would scale up to 10
      const tinyBounds = { width: 50, height: 50 };
      const fittedHigh = fitToBounds(tinyBounds, container);
      expect(fittedHigh.zoom).to.equal(MAX_ZOOM);
    });

    it('handles empty bounds or zero container gracefully', () => {
      const zeroBounds = { width: 0, height: 0 };
      const container = { width: 500, height: 500 };
      const fitted = fitToBounds(zeroBounds, container);
      expect(fitted.zoom).to.equal(1.0);
      expect(fitted.x).to.equal(0);
      expect(fitted.y).to.equal(0);
    });
  });

  describe('panToReveal', () => {
    const container = { width: 800, height: 600 };
    const card = { x: 100, y: 100, width: 240, height: 80 };

    it('is a no-op when the card is already fully visible', () => {
      const current = { x: 0, y: 0, zoom: 1.0 };
      // Card is at [100..340, 100..180], well inside [0..800, 0..600].
      const result = panToReveal(current, card, container);
      expect(result).to.deep.equal(current);
    });

    it('minimally pans right when card is off the left edge', () => {
      // Card x=100. If current.x = -150, card left edge is at 100 - 150 = -50 (offscreen left).
      const current = { x: -150, y: 0, zoom: 1.0 };
      const result = panToReveal(current, card, container);
      // Minimal move brings left edge to padding (or 0): card.x * zoom + newX = 0 => newX = -100.
      expect(result.x).to.equal(-100);
      expect(result.y).to.equal(0);
      expect(result.zoom).to.equal(1.0);
    });

    it('minimally pans left when card is off the right edge', () => {
      // Card x=100, width=240. Right edge is at 340.
      // If container is 300, right edge 340 > 300.
      const smallContainer = { width: 300, height: 600 };
      const current = { x: 0, y: 0, zoom: 1.0 };
      const result = panToReveal(current, card, smallContainer);
      // Minimal move brings right edge to container.width:
      // (card.x + card.width) * zoom + newX = 300 => 340 + newX = 300 => newX = -40.
      expect(result.x).to.equal(-40);
      expect(result.y).to.equal(0);
      expect(result.zoom).to.equal(1.0);
    });

    it('minimally pans down when card is off the top edge', () => {
      // Card y=100. If current.y = -120, top edge is at 100 - 120 = -20 (offscreen top).
      const current = { x: 0, y: -120, zoom: 1.0 };
      const result = panToReveal(current, card, container);
      // Minimal move brings top edge to 0:
      expect(result.y).to.equal(-100);
      expect(result.x).to.equal(0);
      expect(result.zoom).to.equal(1.0);
    });

    it('minimally pans up when card is off the bottom edge', () => {
      // Card y=100, height=80 => bottom edge at 180.
      // If container height is 150, bottom edge 180 > 150.
      const shallowContainer = { width: 800, height: 150 };
      const current = { x: 0, y: 0, zoom: 1.0 };
      const result = panToReveal(current, card, shallowContainer);
      // Minimal move brings bottom edge to container.height:
      // 180 + newY = 150 => newY = -30.
      expect(result.y).to.equal(-30);
      expect(result.x).to.equal(0);
      expect(result.zoom).to.equal(1.0);
    });

    it('respects optional padding when revealing', () => {
      const padding = 20;
      const current = { x: -150, y: 0, zoom: 1.0 };
      const result = panToReveal(current, card, container, padding);
      // Left edge brought to padding=20: 100 + newX = 20 => newX = -80.
      expect(result.x).to.equal(-80);
    });

    it('preserves zoom across pan operations', () => {
      const current = { x: -500, y: -500, zoom: 1.75 };
      const result = panToReveal(current, card, container);
      expect(result.zoom).to.equal(1.75);
    });
  });
});
