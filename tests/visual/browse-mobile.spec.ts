/**
 * /browse never scrolls sideways on a phone (sp-i5e1dm): Jump-to chips and
 * section titles wrap, set tiles are two per row, and every chip, tile and
 * title lies inside the viewport with every section expanded.
 */

import { test, expect } from '@playwright/test';
import { stabilize, settle } from './stabilize';

test('/browse wraps chips, titles and tiles at 390px', async ({ page }) => {
	const width = 390;
	await page.setViewportSize({ width, height: 844 });
	await stabilize(page);
	const response = await page.goto('/browse', { waitUntil: 'domcontentloaded' });
	expect(response?.status() ?? 0).toBeLessThan(400);
	await settle(page, '.grid .tile');

	// Expand every collapsed section.
	for (const hdr of await page.locator('.grouphdr[aria-expanded="false"]').all()) {
		await hdr.click();
	}

	const report = await page.evaluate(() => {
		const outside = (sel: string) =>
			[...document.querySelectorAll<HTMLElement>(sel)]
				.map((e) => e.getBoundingClientRect())
				.filter((r) => r.width > 0 && (r.left < -0.5 || r.right > window.innerWidth + 0.5)).length;
		const grids = [...document.querySelectorAll<HTMLElement>('.grid')].map((g) => ({
			tiles: g.querySelectorAll('.tile').length,
			perRow: new Set(
				[...g.querySelectorAll<HTMLElement>('.tile')].map((t) =>
					Math.round(t.getBoundingClientRect().left)
				)
			).size
		}));
		return {
			scrollWidth: document.documentElement.scrollWidth,
			chips: outside('.navlink'),
			tiles: outside('.grid .tile'),
			titles: outside('.grouphdr, .ghname, .ghmeta'),
			grids
		};
	});

	expect(report.grids.length).toBeGreaterThan(0);
	expect(report.scrollWidth, 'page scrolls sideways').toBeLessThanOrEqual(width);
	expect(report.chips, 'chips outside the viewport').toBe(0);
	expect(report.tiles, 'tiles outside the viewport').toBe(0);
	expect(report.titles, 'section titles outside the viewport').toBe(0);
	for (const g of report.grids) expect(g.perRow).toBe(Math.min(2, g.tiles));
});
