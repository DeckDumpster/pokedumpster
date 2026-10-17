/**
 * /collection's header controls are DeckDumpster's boxes (sp-9s33g3).
 *
 * Ryan: the search bar, grid/list, options, help and columns buttons were all
 * slightly bigger on poke/star than on deck. The expected values below are
 * deck's computed boxes at 390px, from deckdumpster's collection.html:
 *
 *   help      .syntax-help-btn   38 x 38   (fixed)
 *   columns   .cols-btn          38 x 38   (fixed)
 *   toggle    .view-toggle-group 46 x 34   per button: 16px svg + 8/14 padding + 1px border
 *   options   #more-menu-btn     31 x 33   20.8px glyph, line-height 1, 5/10 padding
 *   search    #search-input      42 tall   16px text, 11px vertical padding + 1px border
 *
 * The search's width is what the row leaves over, so it is not asserted.
 * Each must be within 1px; the spec sets its own viewport, so it holds under
 * every project in playwright.config.
 */

import { test, expect, type Page } from '@playwright/test';
import { stabilize } from './stabilize';

const DECK = {
	search: { h: 42 },
	help: { w: 38, h: 38 },
	columns: { w: 38, h: 38 },
	viewTable: { w: 46, h: 34 },
	viewGrid: { w: 46, h: 34 },
	options: { w: 31, h: 33 }
} as const;

const SELECTORS: Record<keyof typeof DECK, string> = {
	search: '[data-testid="search-input"]',
	help: '[data-testid="search-help-link"]',
	columns: '[data-testid="columns-button"]',
	viewTable: '[data-testid="view-table"]',
	viewGrid: '[data-testid="view-grid"]',
	options: 'button.burger'
};

function row(i: number) {
	return {
		printing_id: `t-${i}-normal`,
		card_id: `t-${i}`,
		set_code: 'base1',
		set_name: 'Base Set',
		set_ptcgo_code: 'BS',
		set_symbol_url: null,
		number: String(i + 1),
		name: `Card ${i}`,
		rarity: 'Common',
		supertype: 'Pokémon',
		subtypes: '[]',
		types: '["Fire"]',
		attack_costs: '[]',
		market_price: 1.5,
		image_small: null,
		variant: 'normal',
		owned: true,
		owned_count: 1,
		copies: []
	};
}

async function open(page: Page, width: number) {
	await page.setViewportSize({ width, height: 844 });
	await stabilize(page);
	await page.route('**/api/collection/search*', (route) =>
		route.fulfill({
			json: { rows: Array.from({ length: 5 }, (_, i) => row(i)), total: 5, total_value: 7.5, limit: 5, offset: 0 }
		})
	);
	await page.addInitScript(() => {
		localStorage.clear();
		sessionStorage.clear();
	});
	await page.goto('/collection?q=supertype%3APok%C3%A9mon&all=1');
	await page.locator('[data-testid="view-table"]').waitFor();
}

const box = (page: Page, sel: string) =>
	page.locator(sel).first().evaluate((e) => {
		const r = e.getBoundingClientRect();
		const c = getComputedStyle(e);
		return { w: r.width, h: r.height, padding: c.padding, fontSize: c.fontSize, radius: c.borderRadius };
	});

test('header controls at 390px are within 1px of deck', async ({ page }) => {
	await open(page, 390);
	const failures: string[] = [];
	for (const [name, want] of Object.entries(DECK) as [keyof typeof DECK, { w?: number; h?: number }][]) {
		const got = await box(page, SELECTORS[name]);
		console.log(name, JSON.stringify(got));
		if (want.h !== undefined && Math.abs(got.h - want.h) > 1) failures.push(`${name} height ${got.h} vs deck ${want.h}`);
		if (want.w !== undefined && Math.abs(got.w - want.w) > 1) failures.push(`${name} width ${got.w} vs deck ${want.w}`);
	}
	expect(failures).toEqual([]);
});
