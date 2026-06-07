/**
 * Hand-written implementation for collection_all_cards_sort_interleaves.
 *
 * Regression guard for pokedumpster-ffq (coverage filed as pokedumpster-2o1):
 * in All-cards mode the client-side sort once touched only the aggregated
 * owned rows; unowned printings rendered in a separate trailing {#each} block
 * in raw server order, so changing the sort reordered only the cards you own.
 * The fix folds unowned printings into the same `sorted` list (qty 0), so every
 * sort column interleaves owned + unowned in both grid and table views.
 *
 * We sort by Name: the committed fixture leaves prices null (the Adj./NM
 * columns render "—" for every row), so a price sort can't differentiate, but
 * every row carries a name and the alphabetic order genuinely interleaves
 * owned and unowned cards. The regression is sort-key-agnostic — under the old
 * code the unowned rows trailed in their own block regardless of key.
 */
import type { ReplayHarness } from '../replay';

/** Per-row ownership ('missing' = dimmed unowned) + card name, in DOM order. */
async function readRows(
  h: ReplayHarness,
  rowSelector: string,
): Promise<{ owned: boolean; name: string }[]> {
  return h.page.locator(rowSelector).evaluateAll((els) =>
    els.map((el) => ({
      owned: !el.classList.contains('missing'),
      name: (el.querySelector('.cardname')?.textContent ?? '').trim(),
    })),
  );
}

/**
 * Owned and unowned rows must interleave by the active sort key. The pre-fix
 * layout rendered all owned rows, then all unowned rows as a separate block —
 * so an owned row never followed a missing row. Require both directions.
 */
function assertInterleaved(
  rows: { owned: boolean }[],
  label: string,
): void {
  const seq = rows.map((r) => (r.owned ? 'O' : 'M'));
  const owned = seq.filter((s) => s === 'O').length;
  const missing = seq.filter((s) => s === 'M').length;
  if (owned === 0 || missing === 0) {
    throw new Error(
      `${label}: expected both owned and unowned rows after sort ` +
        `(owned=${owned}, missing=${missing}); All-cards setup wrong`,
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

/** Names must be globally ordered (asc or desc) — proof the sort engaged and
 *  spans owned + unowned together, not a per-block order. */
function assertNameSorted(
  rows: { name: string }[],
  label: string,
): void {
  const names = rows.map((r) => r.name.toLowerCase());
  const asc = names.every((n, i) => i === 0 || names[i - 1]! <= n);
  const desc = names.every((n, i) => i === 0 || names[i - 1]! >= n);
  if (!asc && !desc) {
    throw new Error(
      `${label}: rows not globally name-sorted after clicking Name — ` +
        `owned + unowned are not one sorted list. Names: ${names.join(', ')}`,
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
    .locator('th.sortable', { hasText: 'Name' })
    .first()
    .click({ timeout: 1000 });
  await h.page.waitForTimeout(100);
  const tableRows = await readRows(h, 'table.dd tbody tr');
  assertInterleaved(tableRows, 'table view');
  assertNameSorted(tableRows, 'table view');
  await h.screenshot('table_sorted');

  // ── Grid view ───────────────────────────────────────────────────────
  // Sort state is shared with the table; click the grid Name button to
  // exercise the grid sort path too (direction may flip — interleave holds
  // either way). Grid tiles don't expose the card name, so we assert
  // ownership interleave only here.
  await h.click_by_test_id('view-grid');
  await h.wait_for_visible('.cardgrid .cardtile.missing', 4000);
  await h.page
    .locator('.gridsort .sortbtn', { hasText: 'Name' })
    .first()
    .click({ timeout: 1000 });
  await h.page.waitForTimeout(100);
  const gridRows = await readRows(h, '.cardgrid .cardtile');
  assertInterleaved(gridRows, 'grid view');
  await h.screenshot('grid_sorted');
}
