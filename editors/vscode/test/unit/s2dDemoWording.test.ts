import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

const seed = readFileSync(join(__dirname, '../../demo/s2d/seed.mjs'), 'utf8');

describe('S2D demo proposals', () => {
  it('keeps the three shared mail messages unchanged', () => {
    expect(seed).toContain('Subject: Delivery delay PO-4500182 / lot L-24117');
    expect(seed).toContain('Subject: Booking cut-off week 41, Stuttgart outbound');
    expect(seed).toContain('Subject: Product discontinuation notice: servo drive module SD-40');
  });

  it('names an owner and the next operational step for each approval', () => {
    expect(seed).toMatch(/planning.*confirm.*supplier/i);
    expect(seed).toMatch(/shipping.*confirm.*carrier/i);
    expect(seed).toMatch(/after-sales.*purchase order/i);
  });

  it('does not promise delivery or a system update from approval alone', () => {
    expect(seed).not.toContain('Keep the promised delivery dates of the late orders.');
    expect(seed).not.toContain('Every delivery duty of the consolidated shipments is met.');
  });
});
