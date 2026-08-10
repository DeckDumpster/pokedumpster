/**
 * /collection must not put the whole result set in the DOM (pd-tsqd, pd-7z4o).
 *
 * The reported case is `?q=supertype:Pokémon&all=1` on the real catalog:
 * 56,635 matching printings, and a tab that died rendering them — 289,612 DOM
 * nodes and a 457 MB heap (pd-5vgp). The client holds every one of those rows
 * as a JS object now and renders only the slice under the viewport, so this
 * asserts the property that makes that affordable: the node count is a
 * function of the VIEWPORT, not of how much matched.
 *
 * It also asserts what the paging it replaced was there to protect — the
 * selection surviving a row leaving the rendered window, and the sort staying
 * server-side — because those are exactly what windowing tends to break.
 *
 * And it asserts that the paging is *gone*: no pager control in either view,
 * and the far end of the result a scroll away rather than 149 clicks away
 * (pd-65um, closing pd-lbei by deletion rather than by improvement).
 *
 * The result set here is synthesised by intercepting the search endpoint
 * rather than seeded into the fixture, deliberately: what is under test is the
 * relationship between the result size and the node count, and the assertion
 * is only meaningful when the result is far larger than any fixture.
 * Everything else on the page — binders, decks, the keyword registry, the
 * conditions table — still comes from the real instance.
 *
 * This is the deterministic browser tier's non-visual gate: same Playwright
 * config and same throwaway instance as the screenshot suite, offline and with
 * no Vision call, but it asserts on the DOM rather than on pixels.
 */

import { test, expect, type Page, type Locator } from '@playwright/test';
import { stabilize } from './stabilize';

/** Far more than any browser will render, and not a round number. */
const TOTAL = 56_635;

/** One synthetic owned printing. Distinct name/number per index so a tile is
    traceable back to its position, and every field the list draws is present. */
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

/** Every request the client made, as `{limit, offset, sort, dir}`. */
type Asked = { limit: string | null; offset: string | null; sort: string | null };

/** Serve `/api/collection/search` as the whole of a huge result set, and record
    what was asked for. Generating 56,635 rows per request is too slow for a
    test, so the payload is capped — the client is told the truth about `total`
    and `limit` either way, which is all its arithmetic reads. */
async function serveHugeResult(page: Page, rows = 4_000): Promise<Asked[]> {
	const asked: Asked[] = [];
	await page.route('**/api/collection/search*', (route) => {
		const url = new URL(route.request().url());
		asked.push({
			limit: url.searchParams.get('limit'),
			offset: url.searchParams.get('offset'),
			sort: url.searchParams.get('sort')
		});
		return route.fulfill({
			json: {
				rows: Array.from({ length: rows }, (_, i) => row(i)),
				total: TOTAL,
				limit: TOTAL,
				offset: 0
			}
		});
	});
	return asked;
}

async function openCollection(page: Page, view: 'grid' | 'table'): Promise<Asked[]> {
	await stabilize(page);
	const asked = await serveHugeResult(page);
	// localStorage is per-origin and the suite shares an instance; pin the view
	// and the sort rather than inherit whatever a previous spec left behind.
	await page.addInitScript((v) => {
		localStorage.setItem('collection.view', v);
		localStorage.setItem('collection.sortKey', 'name');
		localStorage.setItem('collection.sortDir', 'asc');
		sessionStorage.clear();
	}, view);
	await page.goto('/collection?q=supertype%3APok%C3%A9mon&all=1');
	await page.locator(view === 'grid' ? '[data-testid="collection-grid"]' : 'table.dd').waitFor();
	return asked;
}

/** How tall the document is — the property the spacers exist to preserve. */
const documentHeight = (page: Page) => page.evaluate(() => document.body.scrollHeight);

/** Scroll the window and let the scroller catch up (it re-windows on rAF). */
async function scrollTo(page: Page, y: number) {
	await page.evaluate((top) => window.scrollTo({ top }), y);
	await page.waitForTimeout(250);
}

/** The one assertion this bead lives or dies by, in both views: a viewport's
    worth of nodes out of a result three orders of magnitude larger. */
async function expectBounded(nodes: Locator) {
	const n = await nodes.count();
	expect(n, `rendered ${n} nodes`).toBeGreaterThan(0);
	expect(n, `rendered ${n} nodes for ${TOTAL} matches`).toBeLessThan(250);
}

test('the client asks for the whole result, not a page of it', async ({ page }) => {
	const asked = await openCollection(page, 'grid');
	expect(asked.length).toBeGreaterThan(0);
	for (const a of asked) {
		expect(a.limit, 'the collection asks for every matching row').toBe('all');
		expect(a.offset, 'there is no paging left to do').toBeNull();
	}
	// And it says so: the count line speaks for the whole result, and can now
	// price it too, because it holds every row it is pricing.
	await expect(page.locator('[data-testid="collection-count"]')).toContainText('56,635 cards');
	await expect(page.locator('[data-testid="collection-count"]')).toContainText('$');
});

test('the grid renders a window, not the result set', async ({ page }) => {
	await openCollection(page, 'grid');
	await expectBounded(page.locator('.cardtile'));
});

test('the table renders a window too', async ({ page }) => {
	await openCollection(page, 'table');
	await expectBounded(page.locator('table.dd tbody tr:not(.vspace)'));
});

/** Nothing anywhere on the page offers to move you a page at a time (pd-65um).
    Checked by every handle the deleted control had — its test ids, its
    landmark, and its wording — because "the pager is gone" has to mean gone,
    not merely drawing nothing for a result that happens to be one page. */
async function expectNoPager(page: Page) {
	await expect(page.locator('[data-testid="pager-position"]')).toHaveCount(0);
	await expect(page.locator('[data-testid="pager-prev"]')).toHaveCount(0);
	await expect(page.locator('[data-testid="pager-next"]')).toHaveCount(0);
	await expect(page.getByRole('navigation', { name: 'Pages' })).toHaveCount(0);
	await expect(page.getByText(/Page \d+ of \d+/)).toHaveCount(0);
}

test('there is no pager on the grid', async ({ page }) => {
	await openCollection(page, 'grid');
	await expectNoPager(page);
	// And still none at the far end, where the bottom pager used to sit.
	await scrollTo(page, await documentHeight(page));
	await expectNoPager(page);
});

test('there is no pager on the table', async ({ page }) => {
	await openCollection(page, 'table');
	await expectNoPager(page);
	await scrollTo(page, await documentHeight(page));
	await expectNoPager(page);
});

test('the far end of the result is reached by scrolling, not by clicking', async ({ page }) => {
	// pd-lbei inverted: under Prev/Next, page 150 of 223 cost 149 clicks. The
	// result is one run now, so the last row it holds is a scroll away and no
	// clicks away. (`serveHugeResult` caps the payload for test speed, so the
	// far end here is row 3,999 of a result the client is told is 56,635 —
	// what is under test is that the end is reachable at all.)
	await openCollection(page, 'grid');
	await scrollTo(page, await documentHeight(page));

	await expect(page.locator('.cardtile').last()).toHaveAttribute('title', /Synthetic 3999\b/);
	// Reaching it did not mean rendering everything between.
	await expectBounded(page.locator('.cardtile'));
});

test('the page is as tall as the whole result, and stays that tall while scrolling', async ({
	page
}) => {
	await openCollection(page, 'grid');
	const before = await documentHeight(page);
	// 4,000 tiles at any plausible tile size is a very tall page. The point is
	// that the height comes from the rows the client holds, not from the nodes
	// it drew.
	expect(before).toBeGreaterThan(20_000);

	await scrollTo(page, before / 2);
	expect(
		Math.abs((await documentHeight(page)) - before),
		'the spacers keep the document the same height as the reader moves'
	).toBeLessThan(4);

	// Still a window, half way down.
	await expectBounded(page.locator('.cardtile'));
});

test('scrolling moves through the result rather than adding to it', async ({ page }) => {
	await openCollection(page, 'grid');
	const first = await page.locator('.cardtile').first().getAttribute('title');
	const before = await page.locator('.cardtile').count();

	await scrollTo(page, 8_000);

	const after = await page.locator('.cardtile').count();
	expect(after, 'the window moved; it did not grow').toBeLessThan(before * 2);
	expect(
		await page.locator('.cardtile').first().getAttribute('title'),
		'different rows are on screen'
	).not.toBe(first);
});

test('a selected copy survives scrolling out of the window and back', async ({ page }) => {
	// The classic thing windowing breaks. The selection is keyed by copy id, so
	// the tile leaving the DOM does not take the selection with it.
	await openCollection(page, 'grid');

	await page.locator('.burger').click();
	await page.locator('.menu .menuItem', { hasText: /^Select$/ }).click();

	await page.locator('.cardtile').first().click();
	await expect(page.locator('.bulkbar .count')).toHaveText('1 selected');

	await scrollTo(page, 12_000);
	// Still selected, and still counted, even though its tile is not rendered.
	await expect(page.locator('.bulkbar .count')).toHaveText('1 selected');
	await expect(page.locator('.cardtile.picked')).toHaveCount(0);

	await scrollTo(page, 0);
	await expect(page.locator('.cardtile.picked')).toHaveCount(1);
});

test('the sort is executed by the server and returns the reader to the top', async ({ page }) => {
	const asked = await openCollection(page, 'grid');
	await scrollTo(page, 10_000);
	expect(await page.evaluate(() => window.scrollY)).toBeGreaterThan(1_000);

	await page.getByRole('button', { name: /^Rarity/ }).click();
	await expect(page.locator('.cardtile').first()).toBeVisible();

	// Re-ordering is a new request, not a re-sort of what is held: a client-side
	// sort of the window would order the wrong rows.
	expect(asked.at(-1)?.sort, `asked: ${JSON.stringify(asked)}`).toBe('rarity');
	expect(asked.at(-1)?.limit).toBe('all');
	// A different order puts different rows where you were looking, so it is
	// read from the top.
	await page.waitForTimeout(250);
	expect(await page.evaluate(() => window.scrollY)).toBeLessThan(50);
});

/* ── The sort surface is scalars only (pd-tjym) ────────────────────────────
 *
 * `value` and `adj` ordered through a subquery joining the tenant's collection
 * to the shared catalog's prices across the ATTACH boundary. SQLite cannot
 * index across attached databases, so ordering by either meant materialising
 * and sorting all 56,635 matches before LIMIT could discard any of them —
 * 1,543 ms of the 2,495 ms first paint this spec's result size stands in for.
 *
 * They are gone from this view for good, not hidden: the assertions below are
 * about what the page OFFERS, because a sort you can still reach is a sort
 * that still costs that. Adj. keeps drawing — computing it for the rows under
 * the viewport was never the expensive part.
 */

test('no sort control on the grid offers value or adj', async ({ page }) => {
	await openCollection(page, 'grid');
	const labels = await page.locator('.gridsort .sortbtn').allInnerTexts();
	expect(labels.length, 'the grid still offers sorts').toBeGreaterThan(0);
	for (const label of labels) {
		expect(label, `grid sort buttons: ${labels.join(', ')}`).not.toMatch(/Value|Adj\./);
	}
});

test('the table has no Value column, and Adj. draws but does not sort', async ({ page }) => {
	await openCollection(page, 'table');
	const headers = await page.locator('table.dd thead th').allInnerTexts();
	expect(headers, `headers: ${headers.join(' | ')}`).not.toContain('Value');

	// Adj. is still a column — it is the last one, and it still draws a figure.
	expect(headers.at(-1)).toMatch(/^Adj\./);
	await expect(page.locator('table.dd tbody tr:not(.vspace)').first()).toBeVisible();

	// But it is not clickable: no `.sortable` header carries it, so there is no
	// affordance to order 56k rows by a column no index can satisfy.
	const sortable = await page.locator('table.dd thead th.sortable').allInnerTexts();
	for (const label of sortable) {
		expect(label, `sortable headers: ${sortable.join(', ')}`).not.toMatch(/Value|Adj\./);
	}
});

test('the endpoint refuses a sort it cannot satisfy, naming the ones it can', async ({ page }) => {
	// Straight at the API — the UI no longer offers these, so this is the guard
	// against them coming back through a bookmarked URL or a stale client.
	for (const key of ['value', 'adj']) {
		const res = await page.request.get(`/api/collection/search?sort=${key}&limit=1`);
		expect(res.status(), `sort=${key}`).toBe(400);
		const body = (await res.json()) as { error: string };
		expect(body.error, `sort=${key}`).toContain('price');
		expect(body.error, `sort=${key}`).toContain('qty');
	}
	// A surviving key still works, so the refusal is about the key and not
	// about `sort=` having stopped being read.
	const ok = await page.request.get('/api/collection/search?sort=price&limit=1');
	expect(ok.status()).toBe(200);
});
