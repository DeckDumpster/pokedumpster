/**
 * Deck groups on a phone: /decks lists the group with its decks and the page
 * does not scroll sideways, and the group page's actions wrap inside the
 * viewport (sp-jcxofv).
 */

import { test, expect } from './fixtures';
import { stabilize, settle } from './stabilize';

const width = 390;

async function visit(page: import('@playwright/test').Page, path: string, waitFor: string) {
	await page.setViewportSize({ width, height: 844 });
	await stabilize(page);
	const response = await page.goto(path, { waitUntil: 'domcontentloaded' });
	expect(response?.status() ?? 0).toBeLessThan(400);
	await settle(page, waitFor);
}

const outside = (page: import('@playwright/test').Page, sel: string) =>
	page.evaluate(
		(s) =>
			[...document.querySelectorAll<HTMLElement>(s)]
				.map((e) => e.getBoundingClientRect())
				.filter((r) => r.width > 0 && (r.left < -0.5 || r.right > window.innerWidth + 0.5)).length,
		sel
	);

test('/decks lists a group above the ungrouped decks at 390px', async ({ page }) => {
	await visit(page, '/decks', '.group');
	await expect(page.locator('.group .groupname')).toHaveText('Starter Decks');
	await expect(page.locator('.group .tile')).toHaveCount(2);
	await expect(page.locator('.ungrouped')).toBeVisible();
	const scrollWidth = await page.evaluate(() => document.documentElement.scrollWidth);
	expect(scrollWidth, 'page scrolls sideways').toBeLessThanOrEqual(width);
	expect(await outside(page, '.tile, .groupname, .groupmeta')).toBe(0);
});

test('/deck-groups/1 keeps its actions and decks inside the viewport at 390px', async ({ page }) => {
	await visit(page, '/deck-groups/1', '.grid .tile');
	await expect(page.locator('h1')).toHaveText('Starter Decks');
	await expect(page.locator('.grid .tile')).toHaveCount(2);
	await page.getByRole('button', { name: 'Delete group' }).click();
	await expect(page.getByRole('alertdialog')).toBeVisible();
	const scrollWidth = await page.evaluate(() => document.documentElement.scrollWidth);
	expect(scrollWidth, 'page scrolls sideways').toBeLessThanOrEqual(width);
	expect(await outside(page, 'button, .tile, .confirm')).toBe(0);
});
