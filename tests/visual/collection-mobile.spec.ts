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
		number: String(101 + i),
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
		await page.getByTestId('columns-drawer').locator('label.pill', { hasText: new RegExp(`^\\s*${label}\\s*$`) }).click();
	}
	await page.mouse.click(5, 400);
	expect((await headers(page)).length).toBe(6);
	const o = await overflow(page);
	expect(o.overflowing, 'cells wider than their column').toBe(0);
	expect(o.ellipsised, 'ellipsised names').toBe(0);
});

/**
 * Density, against DeckDumpster's collection table as measured in its
 * collection.html / shared-card-table.css (sp-rkx1zi): `.main` gutter 4px,
 * `td` padding 2px 12px, a one-line row = 44px thumbnail + 4px padding + 1px
 * rule = 49px. Poke may be as dense or denser, never looser (within 2px).
 */
const DECK = { gutter: 4, tdPadY: 2, tdPadX: 12, rowOneLine: 49, tolerance: 2 };

test('the list is as dense as deckdumpster: gutter, cell padding, one-line row', async ({ page }) => {
	await open(page);
	const m = await page.evaluate(() => {
		const px = (v: string) => parseFloat(v);
		const table = document.querySelector<HTMLElement>('table.dd')!;
		const scroller = table.closest<HTMLElement>('.tableScroll') ?? table.parentElement!;
		const gutter = table.getBoundingClientRect().left;
		const td = document.querySelector<HTMLElement>('table.dd tbody tr:not(.vspace) td')!;
		const cs = getComputedStyle(td);
		const heights = [...document.querySelectorAll<HTMLElement>('table.dd tbody tr:not(.vspace)')].map(
			(r) => r.getBoundingClientRect().height
		);
		return {
			gutter,
			rightGutter: window.innerWidth - table.getBoundingClientRect().right,
			scrollerLeft: scroller.getBoundingClientRect().left,
			padTop: px(cs.paddingTop),
			padBottom: px(cs.paddingBottom),
			padLeft: px(cs.paddingLeft),
			padRight: px(cs.paddingRight),
			shortestRow: Math.min(...heights)
		};
	});
	const tol = DECK.tolerance;
	expect(m.gutter, 'left gutter').toBeLessThanOrEqual(DECK.gutter + tol);
	expect(m.rightGutter, 'right gutter').toBeLessThanOrEqual(DECK.gutter + tol);
	expect(m.padTop, 'td padding-top').toBeLessThanOrEqual(DECK.tdPadY + tol);
	expect(m.padBottom, 'td padding-bottom').toBeLessThanOrEqual(DECK.tdPadY + tol);
	expect(m.padLeft, 'td padding-left').toBeLessThanOrEqual(DECK.tdPadX + tol);
	expect(m.padRight, 'td padding-right').toBeLessThanOrEqual(DECK.tdPadX + tol);
	expect(m.shortestRow, 'one-line row height').toBeLessThanOrEqual(DECK.rowOneLine + tol);
});

/**
 * Columns size to content (sp-pddr2o): Energy and # are each no wider than
 * their widest cell content + 2x the td padding + 2px, and the name column
 * takes the rest, so "Psychic Energy" stays on one line.
 */
test('Energy and # shrink to their content and short names do not wrap', async ({ page }) => {
	await open(page);
	await page.route('**/api/collection/search*', (route) =>
		route.fulfill({
			json: {
				rows: Array.from({ length: 40 }, (_, i) => ({ ...row(i), name: 'Psychic Energy', supertype: 'Energy', subtypes: '["Basic"]' })),
				total: 40,
				total_value: 60,
				limit: 40,
				offset: 0
			}
		})
	);
	await page.reload();
	await page.locator('table.dd tbody tr:not(.vspace)').first().waitFor();
	await page.getByTestId('columns-button').click();
	for (const label of ['Energy', '#']) {
		await page.getByTestId('columns-drawer').locator('label.pill', { hasText: new RegExp(`^\\s*${label}\\s*$`) }).click();
	}
	await page.mouse.click(5, 400);
	const m = await page.evaluate(() => {
		const ths = [...document.querySelectorAll<HTMLElement>('table.dd thead th')];
		const out: Record<string, { width: number; content: number; pad: number }> = {};
		ths.forEach((th, idx) => {
			const label = th.innerText.trim().toUpperCase().replace(/[▲▼\s]/g, '');
			if (label !== 'ENERGY' && label !== '#') return;
			const cells = [th, ...document.querySelectorAll<HTMLElement>(`table.dd tbody tr:not(.vspace) td:nth-child(${idx + 1})`)];
			let content = 0;
			for (const c of cells) {
				const r = document.createRange();
				r.selectNodeContents(c);
				content = Math.max(content, r.getBoundingClientRect().width);
			}
			const cs = getComputedStyle(cells[1]);
			out[label] = {
				width: th.getBoundingClientRect().width,
				content,
				pad: parseFloat(cs.paddingLeft) + parseFloat(cs.paddingRight)
			};
		});
		const name = document.querySelector<HTMLElement>('table.dd tbody .cardname')!;
		const lh = parseFloat(getComputedStyle(name).lineHeight);
		return { out, lines: Math.round(name.getBoundingClientRect().height / lh) };
	});
	for (const k of ['ENERGY', '#']) {
		const c = m.out[k];
		expect(c, `${k} column present`).toBeTruthy();
		expect(c.width, `${k} column width`).toBeLessThanOrEqual(c.content + c.pad + 2);
	}
	expect(m.lines, 'Psychic Energy lines').toBe(1);
});
