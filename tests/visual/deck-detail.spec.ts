/**
 * /decks/[id] groups cards by supertype like deckdumpster's deck page
 * (sp-vuzv2c): a section per supertype with its count, lines carrying quantity,
 * name and at-a-glance attributes, a link to the exact printing@condition, and
 * no set / collector number / condition text. No sideways scroll on a phone.
 */

import { test, expect } from './fixtures';
import { stabilize, settle } from './stabilize';

for (const [label, width, height] of [
	['desktop', 1440, 900],
	['phone', 390, 844]
] as const) {
	test(`/decks/1 is grouped by supertype at ${label}`, async ({ page }) => {
		await page.setViewportSize({ width, height });
		await stabilize(page);
		const response = await page.goto('/decks/1', { waitUntil: 'domcontentloaded' });
		expect(response?.status() ?? 0).toBeLessThan(400);
		await settle(page, '.deck section');

		const sections = await page.locator('.deck section h2').allInnerTexts();
		expect(sections.length).toBeGreaterThan(0);
		for (const h of sections) expect(h).toMatch(/^(Pok.mon|Trainer|Energy|Other)\s+\d+$/);

		const lines = page.locator('.deck li');
		expect(await lines.count()).toBeGreaterThan(0);
		for (const li of await lines.all()) {
			await expect(li.locator('.qty')).toHaveText(/^\d+×$/);
			await expect(li.locator('a')).toHaveAttribute('href', /^\/card\/[^/]+\/[^?]+\?printing=.+&condition=.+/);
		}

		const text = (await page.locator('.deck').innerText()).replace(/\d+ HP/g, '');
		expect(text).not.toMatch(/\b(Near Mint|Lightly Played|Moderately Played|Heavily Played|Damaged)\b/i);
		expect(text).not.toMatch(/\b(NM|LP|MP|HP|DMG)\b/);
		expect(await page.locator('.deck table, .deck th').count()).toBe(0);

		expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);

		await page.locator('.deck li a').first().hover();
		await expect(page.locator('.deck .preview')).toBeVisible();
		await page.screenshot({ path: `test-results/deck-detail-${label}.png`, fullPage: true });
	});
}
