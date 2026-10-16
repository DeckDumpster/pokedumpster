/**
 * /collection's list view at 390px is DeckDumpster's (sp-in6ned): two columns
 * by default — Card (thumbnail, name wrapping onto a second line) and Type
 * (type line, subtype beneath) — with every other column behind the chooser.
 * Nothing is ellipsised and no cell overflows its column.
 */

import { test, expect, type Page } from '@playwright/test';
import { stabilize } from './stabilize';

const NAMES = [
	'Professor Sada’s Vitality Special Illustration Rare',
	'Abomasnow',
	'Ancient Booster Energy Capsule'
];

function row(i: number) {
	return {
		printing_id: `m-${i}-normal`,
		card_id: `m-${i}`,
		set_code: 'base1',
		set_name: 'Base Set',
		set_ptcgo_code: 'BS',
		set_symbol_url: null,
		number: String(i + 1),
		name: `${NAMES[i % NAMES.length]} ${i}`,
		rarity: 'Common',
		supertype: 'Pokémon',
		subtypes: '["Stage 2","VSTAR"]',
		types: '["Fire"]',
		attack_costs: JSON.stringify([
			{ name: 'Hyper Beam', cost: ['Fire', 'Fire', 'Colorless', 'Colorless', 'Colorless'] }
		]),
		market_price: 1.5,
		image_small: null,
		variant: 'normal',
		owned: true,
		owned_count: 1,
		copies: [
			{
				id: 1000 + i,
				condition: 'Near Mint',
				language: 'English',
				status: 'owned',
				graded: false,
				purchase_price: null,
				acquired_at: '2025-01-01',
				binder_id: null,
				deck_id: null
			}
		]
	};
}

async function open(page: Page) {
	await page.setViewportSize({ width: 390, height: 844 });
	await stabilize(page);
	await page.route('**/api/collection/search*', (route) =>
		route.fulfill({
			json: {
				rows: Array.from({ length: 40 }, (_, i) => row(i)),
				total: 40,
				total_value: 60,
				limit: 40,
				offset: 0
			}
		})
	);
	await page.addInitScript(() => {
		localStorage.clear();
		localStorage.setItem('collection.view', 'table');
		localStorage.setItem('collection.sortKey', 'name');
		localStorage.setItem('collection.sortDir', 'asc');
		sessionStorage.clear();
	});
	await page.goto('/collection?q=supertype%3APok%C3%A9mon&all=1');
	await page.locator('table.dd tbody tr:not(.vspace)').first().waitFor();
}

const headers = (page: Page) =>
	page.locator('table.dd thead th').allInnerTexts().then((h) => h.map((t) => t.trim().toUpperCase()));

/** Cells whose content is wider than the cell, and names that are ellipsised. */
const overflow = (page: Page) =>
	page.evaluate(() => {
		const cells = [...document.querySelectorAll<HTMLElement>('table.dd tbody tr:not(.vspace) td')];
		return {
			cells: cells.length,
			overflowing: cells.filter((c) => c.scrollWidth > c.clientWidth + 1).length,
			outside: cells.filter((c) => c.getBoundingClientRect().right > window.innerWidth + 0.5).length,
			ellipsised: [...document.querySelectorAll<HTMLElement>('table.dd .namebody, table.dd .cardname')].filter(
				(e) => getComputedStyle(e).textOverflow === 'ellipsis'
			).length,
			pageWidth: document.documentElement.scrollWidth
		};
	});

test('the list opens with Card and Type, wrapped names, nothing clipped', async ({ page }) => {
	await open(page);
	expect(await headers(page)).toEqual(['CARD ▲', 'TYPE']);
	const o = await overflow(page);
	expect(o.cells).toBeGreaterThan(0);
	expect(o.overflowing, 'cells wider than their column').toBe(0);
	expect(o.outside, 'cells past the viewport edge').toBe(0);
	expect(o.ellipsised, 'ellipsised names').toBe(0);
	expect(o.pageWidth, 'page scrolls sideways').toBeLessThanOrEqual(390);
	// The long name takes more than one line rather than being cut.
	const lines = await page
		.locator('table.dd tbody .cardname')
		.first()
		.evaluate((e) => Math.round(e.getBoundingClientRect().height / parseFloat(getComputedStyle(e).lineHeight)));
	expect(lines).toBeGreaterThan(1);
});

test('the column chooser brings the rest back, and still nothing overflows', async ({ page }) => {
	await open(page);
	await page.getByTestId('columns-button').click();
	for (const label of ['Qty', 'Cost', 'Set', 'Price']) {
		await page.getByTestId('columns-menu').getByLabel(label, { exact: true }).check();
	}
	await page.mouse.click(5, 400);
	expect((await headers(page)).length).toBe(6);
	const o = await overflow(page);
	expect(o.overflowing, 'cells wider than their column').toBe(0);
	expect(o.ellipsised, 'ellipsised names').toBe(0);
});
