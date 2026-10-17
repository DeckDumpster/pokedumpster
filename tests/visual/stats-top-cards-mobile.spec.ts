/**
 * The set stats "Most valuable" list keeps the card name legible on a phone
 * (sp-q01et5): the name takes the remaining width and wraps, number, rarity
 * and the master-only tag sit on a second line, and no row scrolls sideways.
 */

import { test, expect } from '@playwright/test';
import { stabilize } from './stabilize';

const TOP = [
	{
		card_id: 'x-1',
		number: 'TG30',
		name: 'Mega Charizard X ex',
		rarity: 'Special Illustration Rare',
		image_small: null,
		price: 1234.56,
		owned: false,
		master_only: true
	},
	{
		card_id: 'x-2',
		number: '12',
		name: 'Mew ex',
		rarity: 'Common',
		image_small: null,
		price: 9.5,
		owned: true,
		master_only: false
	}
];

// A phone, whichever project runs this: both projects configure a wider
// viewport, and the page must be created at 390px rather than resized to it.
const width = 390;
test.use({ viewport: { width, height: 844 } });

test('Most valuable rows show the full name at 390px', async ({ page }) => {
	expect(page.viewportSize()?.width).toBe(width);
	await stabilize(page);
	await page.route('**/api/sets/*/top-cards*', (route) => route.fulfill({ json: TOP }));
	const response = await page.goto('/browse/sv3pt5/stats', { waitUntil: 'domcontentloaded' });
	expect(response?.status() ?? 0).toBeLessThan(400);
	const rows = page.locator('[data-testid="top-cards"] .toprow');
	await expect(rows).toHaveCount(TOP.length);

	const report = await page.evaluate(() => {
		const rows = [...document.querySelectorAll<HTMLElement>('[data-testid="top-cards"] .toprow')];
		return {
			scrollWidth: document.documentElement.scrollWidth,
			wide: [...document.querySelectorAll<HTMLElement>('body *')]
				.filter((e) => e.getBoundingClientRect().right > window.innerWidth + 0.5)
				.map((e) => e.tagName + '.' + e.className)
				.slice(0, 8),
			rows: rows.map((r) => {
				const n = r.querySelector<HTMLElement>('.topname')!;
				const box = r.getBoundingClientRect();
				return {
					clipped: n.scrollWidth > n.clientWidth,
					nameWidth: n.getBoundingClientRect().width,
					rowOverflow: r.scrollWidth > r.clientWidth || box.right > window.innerWidth + 0.5,
					metaBelow:
						r.querySelector('.topmeta')!.getBoundingClientRect().top >=
						n.getBoundingClientRect().bottom - 1
				};
			})
		};
	});

	expect(report.scrollWidth, `page scrolls sideways: ${report.wide.join(' ')}`).toBeLessThanOrEqual(width);
	for (const r of report.rows) {
		expect(r.clipped, 'name clipped').toBe(false);
		expect(r.nameWidth, 'name collapsed').toBeGreaterThan(100);
		expect(r.rowOverflow, 'row overflows').toBe(false);
		expect(r.metaBelow, 'meta not under the name').toBe(true);
	}
});
