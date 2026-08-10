/**
 * /collection must not put the whole result set in the DOM (pd-tsqd).
 *
 * The reported case is `?q=supertype:Pokémon&all=1` on the real catalog:
 * 56,635 matching printings, a 44 MB body, and a tab that dies rendering it.
 * The endpoint answers in bounded pages now (pd-jsby), and this asserts the
 * client half — that the page renders one page, states which one, and can
 * reach the rest without ever holding it all.
 *
 * The result set here is synthesised by intercepting the search endpoint
 * rather than seeded into the fixture, deliberately: what is under test is
 * the relationship between `total` and the node count, and the assertion is
 * only meaningful when `total` is far larger than any fixture. Everything
 * else on the page — binders, decks, the keyword registry, the conditions
 * table — still comes from the real instance.
 *
 * This is the deterministic browser tier's non-visual gate: same Playwright
 * config and same throwaway instance as the screenshot suite, offline and
 * with no Vision call, but it asserts on the DOM rather than on pixels.
 */

import { test, expect, type Page } from '@playwright/test';
import { stabilize } from './stabilize';

/** Far more than any browser will render, and not a multiple of the page. */
const TOTAL = 56_635;
/** What the server bounds itself to — `search::DEFAULT_LIMIT`. */
const LIMIT = 250;
const PAGES = Math.ceil(TOTAL / LIMIT);

/** One synthetic owned printing. Distinct name/number per index so a tile is
    traceable back to its offset, and every field the list draws is present. */
function row(i: number) {
	return {
		printing_id: `synthetic-${i}-normal`,
		card_id: `synthetic-${i}`,
		set_code: 'base1',
		set_name: 'Base Set',
		set_ptcgo_code: 'BS',
		set_symbol_url: null,
		number: String(i + 1),
		name: `Synthetic ${i}`,
		rarity: 'Common',
		supertype: 'Pokémon',
		subtypes: '["Basic"]',
		types: '["Fire"]',
		attack_costs: null,
		market_price: 1.5,
		image_small: null,
		variant: 'normal',
		owned: true,
		owned_count: 1,
		copies: [
			{
				id: 100_000 + i,
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

/** Serve `/api/collection/search` as a bounded page of a huge result set, and
    record the offsets asked for. */
async function serveHugeResult(page: Page): Promise<number[]> {
	const offsets: number[] = [];
	await page.route('**/api/collection/search*', (route) => {
		const url = new URL(route.request().url());
		const offset = Number(url.searchParams.get('offset') ?? 0);
		// The client must never ask for the whole thing. If it did, this would
		// be the request that produced the 44 MB body.
		const limit = Number(url.searchParams.get('limit') ?? LIMIT);
		expect(limit, 'the client asked for an unbounded page').toBeLessThanOrEqual(LIMIT);
		offsets.push(offset);
		const rows = Array.from({ length: Math.min(LIMIT, TOTAL - offset) }, (_, i) =>
			row(offset + i)
		);
		return route.fulfill({ json: { rows, total: TOTAL, limit: LIMIT, offset } });
	});
	return offsets;
}

/** The grid at page one of the synthetic result. */
async function openGrid(page: Page): Promise<number[]> {
	await stabilize(page);
	const offsets = await serveHugeResult(page);
	// localStorage is per-origin and the suite shares an instance; pin the view
	// rather than inherit whatever a previous spec left behind.
	await page.addInitScript(() => localStorage.setItem('collection.view', 'grid'));
	await page.goto('/collection?q=supertype%3APok%C3%A9mon&all=1');
	await page.locator('[data-testid="collection-grid"]').waitFor({ state: 'visible' });
	return offsets;
}

test('the grid renders one page, not the result set', async ({ page }) => {
	await openGrid(page);

	// The whole bead in one assertion: 56,635 matches, at most one page of
	// tiles. Not "fewer than 56,635" — bounded, and bounded by the page.
	await expect(page.locator('.cardtile')).toHaveCount(LIMIT);

	// And the count still speaks for the whole result, so the bound is not
	// hiding how much matched.
	await expect(page.locator('[data-testid="collection-count"]')).toContainText('56,635 cards');
	await expect(page.locator('[data-testid="pager-position"]').first()).toContainText(
		`Page 1 of ${PAGES}`
	);
});

test('the table renders one page too', async ({ page }) => {
	await stabilize(page);
	await serveHugeResult(page);
	await page.addInitScript(() => localStorage.setItem('collection.view', 'table'));
	await page.goto('/collection?q=supertype%3APok%C3%A9mon&all=1');
	await expect(page.locator('table.dd tbody tr')).toHaveCount(LIMIT);
});

test('Next fetches the next page and leaves the old one behind', async ({ page }) => {
	const offsets = await openGrid(page);
	await expect(page.locator('.cardtile')).toHaveCount(LIMIT);

	await page.locator('[data-testid="pager-next"]').first().click();

	await expect(page.locator('[data-testid="pager-position"]').first()).toContainText(
		`Page 2 of ${PAGES}`
	);
	// Still one page of tiles — the second page replaced the first rather than
	// being appended to it. An infinite scroll would read 500 here, and would
	// be back to O(n) in the result size a few pages later.
	await expect(page.locator('.cardtile')).toHaveCount(LIMIT);
	expect(offsets.at(-1), `offsets requested: ${offsets.join(', ')}`).toBe(LIMIT);

	// The page is addressable: a reload lands on it, not back at the top.
	await expect(page).toHaveURL(/offset=250/);
});

test('a selected copy stays selected across a page turn', async ({ page }) => {
	// The classic thing paging breaks. The selection is keyed by copy id, so
	// the row scrolling out of the result does not take the selection with it.
	await openGrid(page);

	await page.locator('.burger').click();
	await page.locator('.menu .menuItem', { hasText: /^Select$/ }).click();

	await page.locator('.cardtile').first().click();
	await expect(page.locator('.bulkbar .count')).toHaveText('1 selected');

	await page.locator('[data-testid="pager-next"]').first().click();
	await expect(page.locator('[data-testid="pager-position"]').first()).toContainText('Page 2');
	// Still selected, and still counted, even though its row is not on screen.
	await expect(page.locator('.bulkbar .count')).toHaveText('1 selected');
	await expect(page.locator('.cardtile.picked')).toHaveCount(0);

	await page.locator('[data-testid="pager-prev"]').first().click();
	await expect(page.locator('[data-testid="pager-position"]').first()).toContainText('Page 1');
	await expect(page.locator('.cardtile.picked')).toHaveCount(1);
});

test('a deep-linked page loads that page, and sorting returns to the first', async ({ page }) => {
	await stabilize(page);
	const offsets = await serveHugeResult(page);
	await page.addInitScript(() => localStorage.setItem('collection.view', 'grid'));
	await page.goto('/collection?q=supertype%3APok%C3%A9mon&all=1&offset=1000');

	await expect(page.locator('[data-testid="pager-position"]').first()).toContainText('Page 5');
	expect(offsets, 'the deep link is what was fetched').toContain(1000);

	// Re-ordering the whole result makes the page you were on a different 250
	// rows, so it starts at the top of the new order.
	await page.getByRole('button', { name: /^Rarity/ }).click();
	await expect(page.locator('[data-testid="pager-position"]').first()).toContainText('Page 1');
	await expect(page).not.toHaveURL(/offset=/);
});
