/**
 * Hand-written implementation for collection_all_cards_sort_interleaves.
 *
 * Regression guard for pokedumpster-ffq (coverage filed as pokedumpster-2o1):
 * in All-cards mode the client-side sort once touched only the aggregated
 * owned rows; unowned printings rendered in a separate trailing {#each} block
 * in raw server order, so changing the sort — a price column — reordered only
 * the cards you own. The fix folds unowned printings into the same `sorted`
 * list (qty 0), so every sort column interleaves owned + unowned in both grid
 * and table views.
 *
 * We sort by NM, the Near Mint market price. The bug report used the
 * condition-adjusted Adj. column, which is no longer sortable — it ordered
 * through a join across the ATTACH boundary that no index can satisfy
 * (pd-tjym) — and NM is the surviving price column that exercises the same
 * path. The fixture mixes cheap and expensive cards across both ownership
 * states (owned Base Set holos at $100-$320, unowned SIRs at $130-$280, plus
 * sub-$1 commons of each), so the price order genuinely interleaves them.
 * Asserts (a) owned + unowned interleave in DOM order and (b) the NM column is
 * globally price-sorted — proof the two are one list.
 */
import type { ReplayHarness } from '../replay';

interface Row {
  owned: boolean;
  /** NM column value, or null for a "—" cell. */
  price: number | null;
}

/** Read each table row's ownership + NM price (the second-to-last cell — Adj.
 *  is last and still draws, it just no longer sorts), in DOM order. */
async function readTableRows(h: ReplayHarness): Promise<Row[]> {
  return h.page.locator('table.dd tbody tr').evaluateAll((els) =>
    els.map((el) => {
      const cell = el.querySelector('td:nth-last-child(2)'); // NM (Adj. is last)
      const txt = (cell?.textContent ?? '').trim();
      const num = Number(txt.replace(/[^0-9.]/g, ''));
      return {
        owned: !el.classList.contains('missing'),
        price: txt === '' || txt === '—' || Number.isNaN(num) ? null : num,
      };
    }),
  );
}

/** Read each grid tile's ownership (tiles don't expose price), in DOM order. */
async function readGridOwnership(h: ReplayHarness): Promise<boolean[]> {
  return h.page
    .locator('.cardgrid .cardtile')
    .evaluateAll((els) => els.map((el) => !el.classList.contains('missing')));
}

/**
 * Owned and unowned rows must interleave by the active sort key. The pre-fix
 * layout rendered all owned rows, then all unowned rows as a separate block —
 * so an owned row never followed a missing row. Require both directions.
 */
function assertInterleaved(owned: boolean[], label: string): void {
  const seq = owned.map((o) => (o ? 'O' : 'M'));
  const nOwned = seq.filter((s) => s === 'O').length;
  const nMissing = seq.filter((s) => s === 'M').length;
  if (nOwned === 0 || nMissing === 0) {
    throw new Error(
      `${label}: expected both owned and unowned rows after sort ` +
        `(owned=${nOwned}, missing=${nMissing}); All-cards setup wrong`,
    );
  }
  const firstMissing = seq.indexOf('M');
  const lastOwned = seq.lastIndexOf('O');
  const firstOwned = seq.indexOf('O');
  const lastMissing = seq.lastIndexOf('M');
  // owned-after-missing is the discriminator: false when unowned form a
  // separate trailing block (the ffq regression).
  if (!(firstMissing < lastOwned && firstOwned < lastMissing)) {
    throw new Error(
      `${label}: owned + unowned not interleaved by sort — looks like two ` +
        `separate blocks (ffq regression). Sequence: ${seq.join('')}`,
    );
  }
}

/** The NM column must be globally ordered (asc or desc) — proof the sort
 *  engaged and spans owned + unowned together, not a per-block order. */
function assertPriceSorted(rows: Row[], label: string): void {
  const prices = rows.map((r) => r.price);
  if (prices.some((p) => p === null)) {
    throw new Error(
      `${label}: some rows show no NM price ("—") — the fixture price join ` +
        `regressed (see pokedumpster-qm9). Prices: ${prices.join(', ')}`,
    );
  }
  const nums = prices as number[];
  const asc = nums.every((p, i) => i === 0 || nums[i - 1]! <= p);
  const desc = nums.every((p, i) => i === 0 || nums[i - 1]! >= p);
  if (!asc && !desc) {
    throw new Error(
      `${label}: NM column not globally price-sorted — owned + unowned are ` +
        `not one sorted list. Prices: ${nums.join(', ')}`,
    );
  }
}

export async function steps(h: ReplayHarness) {
  await h.wait_for_visible('[data-testid="search-input"]');

  // Turn on All-cards so unowned printings render alongside owned ones.
  if (!(await h.page.getByTestId('all-cards-toggle').isChecked())) {
    await h.click_by_test_id('all-cards-toggle');
  }
  // The toggle refetches with include_unowned; wait for missing rows to land.
  await h.wait_for_visible('.cardtile.missing, tr.missing', 6000);

  // ── Table view ──────────────────────────────────────────────────────
  await h.click_by_test_id('view-table');
  await h.wait_for_visible('table.dd tbody tr.missing', 4000);
  await h.page
    .locator('th.sortable', { hasText: /^NM/ })
    .first()
    .click({ timeout: 1000 });
  await h.page.waitForTimeout(100);
  const tableRows = await readTableRows(h);
  assertInterleaved(
    tableRows.map((r) => r.owned),
    'table view',
  );
  assertPriceSorted(tableRows, 'table view');
  await h.screenshot('table_sorted');

  // ── Grid view ───────────────────────────────────────────────────────
  // Sort state is shared with the table; click the grid NM button to
  // exercise the grid sort path too (direction may flip — interleave holds
  // either way). Grid tiles don't expose the price, so we assert ownership
  // interleave only here.
  await h.click_by_test_id('view-grid');
  await h.wait_for_visible('.cardgrid .cardtile.missing', 4000);
  await h.page
    .locator('.gridsort .sortbtn', { hasText: /^NM/ })
    .first()
    .click({ timeout: 1000 });
  await h.page.waitForTimeout(100);
  assertInterleaved(await readGridOwnership(h), 'grid view');
  await h.screenshot('grid_sorted');
}
