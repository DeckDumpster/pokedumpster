/**
 * The browse set page has ONE tile (the kit's CardTile): image box, then a footer that
 * is the collector number and the printing pips and nothing else. The footer
 * is one line at every column count, every tile's pips sit at the same y
 * across a row, and a checkbox decides whether landscape cards are turned into
 * the portrait pocket. There is no Binder/Card control.
 *
 * Card art is stubbed with generated SVGs (alternating portrait/landscape by
 * URL) so the landscape arm has something to act on whatever the fixture holds.
 */

import { test, expect } from './fixtures';
import type { Page } from '@playwright/test';
import { stabilize } from './stabilize';

const SET = '/browse/base1';

function svg(w: number, h: number): string {
	return `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}"><rect width="${w}" height="${h}" fill="#468"/></svg>`;
}

async function stubArt(page: Page) {
	const seen = new Map<string, number>();
	await page.route(/\.(png|jpg|jpeg|webp)(\?.*)?$/, async (route) => {
		const url = route.request().url();
		if (!/images\.pokemontcg\.io/.test(url)) return route.continue();
		if (!seen.has(url)) seen.set(url, seen.size);
		const landscape = seen.get(url)! % 2 === 0;
		await route.fulfill({
			contentType: 'image/svg+xml',
			body: landscape ? svg(700, 500) : svg(500, 700),
		});
	});
}

async function open(page: Page, qs: string) {
	await page.setViewportSize({ width: 390, height: 844 });
	// Playwright tries the newest route first: stabilize()'s catch-all would
	// answer every card image with a portrait placeholder if it came last.
	await stabilize(page);
	await stubArt(page);
	await page.goto(`${SET}?${qs}`);
	await page.locator('.card-tile').first().waitFor();
	await page.waitForLoadState('networkidle');
}

test.describe('browse tile', () => {
	for (const cols of [2, 3, 4, 6]) {
		test(`footer is one line, aligned, inside its tile at ${cols} cols`, async ({ page }) => {
			await open(page, `cols=${cols}`);
			const tiles = await page.locator('.card-tile').evaluateAll((els) =>
				els.map((el) => {
					const t = el.getBoundingClientRect();
					const f = el.querySelector('.footer')!.getBoundingClientRect();
					const fs = el.querySelector('.footer')!;
					const chip = el.querySelector('.pip')?.getBoundingClientRect() ?? null;
					const art = el.querySelector('.art')!.getBoundingClientRect();
					return {
						top: Math.round(t.top),
						foot: { top: f.top, bottom: f.bottom, left: f.left, right: f.right },
						scrollW: fs.scrollWidth,
						clientW: fs.clientWidth,
						tile: { left: t.left, right: t.right, bottom: t.bottom },
						chip: chip && { top: chip.top, h: chip.height },
						artTop: art.top,
						artH: art.height,
					};
				}),
			);
			expect(tiles.length).toBeGreaterThan(cols);
			for (const t of tiles) {
				expect(t.foot.bottom - t.foot.top, 'footer is a single line').toBeLessThanOrEqual(24.5);
				expect(t.scrollW, 'footer content fits').toBeLessThanOrEqual(t.clientW + 1);
				expect(t.foot.left).toBeGreaterThanOrEqual(t.tile.left - 0.5);
				expect(t.foot.right).toBeLessThanOrEqual(t.tile.right + 0.5);
			}
			const rows = new Map<number, typeof tiles>();
			for (const t of tiles) rows.set(t.top, [...(rows.get(t.top) ?? []), t]);
			for (const row of rows.values()) {
				for (const t of row) {
					expect(t.artH).toBeCloseTo(row[0].artH, 0);
					expect(t.foot.top).toBeCloseTo(row[0].foot.top, 0);
					if (t.chip && row[0].chip) expect(t.chip.top).toBeCloseTo(row[0].chip.top, 0);
				}
			}
		});
	}

	test('no Binder/Card control, and a stale ?view= is ignored', async ({ page }) => {
		const errors: string[] = [];
		page.on('pageerror', (e) => errors.push(String(e)));
		await open(page, 'view=card');
		await expect(page.getByRole('button', { name: /^Card$/ })).toHaveCount(0);
		await expect(page.getByRole('button', { name: /^Binder$/ })).toHaveCount(0);
		await expect(page.locator('.footer .name')).toHaveCount(0);
		expect(errors).toEqual([]);
	});

	test('no "Turn leaders and bases" control — Pokémon has none', async ({ page }) => {
		await open(page, 'cols=3');
		await expect(page.getByLabel('Turn leaders and bases')).toHaveCount(0);
		await expect(page.getByText('Turn leaders and bases')).toHaveCount(0);
		await expect(page.locator('.card-tile.rotate')).toHaveCount(0);
	});
});
