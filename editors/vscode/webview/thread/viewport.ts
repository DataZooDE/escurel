export const MIN_ZOOM = 0.4;
export const MAX_ZOOM = 2.5;

export interface ViewportState {
  x: number;
  y: number;
  zoom: number;
}

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface Size {
  width: number;
  height: number;
}

export interface Point {
  x: number;
  y: number;
}

/** Clamps zoom factor within readable and navigable scale limits. */
export function clampZoom(zoom: number): number {
  return Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, zoom));
}

/**
 * Adjusts viewport translation so that the canvas point under the cursor
 * remains at the same container position before and after scaling.
 */
export function zoomAboutPoint(
  current: ViewportState,
  cursor: Point,
  targetZoom: number,
): ViewportState {
  const nextZoom = clampZoom(targetZoom);
  if (current.zoom === 0) {
    return { x: current.x, y: current.y, zoom: nextZoom };
  }
  const scaleRatio = nextZoom / current.zoom;
  return {
    x: cursor.x - (cursor.x - current.x) * scaleRatio,
    y: cursor.y - (cursor.y - current.y) * scaleRatio,
    zoom: nextZoom,
  };
}

/**
 * Calculates a uniform zoom and translation that centers the entire graph
 * within the visible canvas container.
 */
export function fitToBounds(bounds: Size, container: Size, padding = 0): ViewportState {
  if (bounds.width <= 0 || bounds.height <= 0 || container.width <= 0 || container.height <= 0) {
    return { x: 0, y: 0, zoom: 1.0 };
  }
  const availableWidth = Math.max(1, container.width - padding * 2);
  const availableHeight = Math.max(1, container.height - padding * 2);

  const scaleX = availableWidth / bounds.width;
  const scaleY = availableHeight / bounds.height;
  const zoom = clampZoom(Math.min(scaleX, scaleY));

  const scaledWidth = bounds.width * zoom;
  const scaledHeight = bounds.height * zoom;

  return {
    x: (container.width - scaledWidth) / 2,
    y: (container.height - scaledHeight) / 2,
    zoom,
  };
}

/**
 * Computes minimal translation required to bring a card within the visible viewport bounds
 * without altering current zoom level or shifting already visible cards.
 */
export function panToReveal(
  current: ViewportState,
  card: Rect,
  container: Size,
  padding = 0,
): ViewportState {
  const cardLeft = card.x * current.zoom + current.x;
  const cardRight = (card.x + card.width) * current.zoom + current.x;
  const cardTop = card.y * current.zoom + current.y;
  const cardBottom = (card.y + card.height) * current.zoom + current.y;

  const minX = padding;
  const maxX = container.width - padding;
  const minY = padding;
  const maxY = container.height - padding;

  let nextX = current.x;
  let nextY = current.y;

  // Move right if clipped on the left; move left only if right edge exceeds container.
  if (cardLeft < minX) {
    nextX = current.x + (minX - cardLeft);
  } else if (cardRight > maxX) {
    nextX = current.x - (cardRight - maxX);
  }

  // Move down if clipped on top; move up only if bottom edge exceeds container.
  if (cardTop < minY) {
    nextY = current.y + (minY - cardTop);
  } else if (cardBottom > maxY) {
    nextY = current.y - (cardBottom - maxY);
  }

  return {
    x: nextX,
    y: nextY,
    zoom: current.zoom,
  };
}
