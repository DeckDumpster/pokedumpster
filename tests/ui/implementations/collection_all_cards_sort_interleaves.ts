/**
 * Hand-written implementation for collection_all_cards_sort_interleaves.
 *
 * Regression guard for pokedumpster-ffq (coverage filed as pokedumpster-2o1):
 * in All-cards mode the client-side sort once touched only the aggregated
 * owned rows; unowned printings rendered in a separate trailing {#each} block
 * in raw server order, so changing the sort — e.g. Adj. price — reordered only
 * the cards you own. The fix folds unowned printings into the same `sorted`
 * list (qty 0), so every sort column interleaves owned + unowned in both grid
 * and table views.
 *
 * We sort by the Adj. (condition-adjusted price) column — the exact field from
 * the bug report. The fixture mixes cheap and expensive cards across both
 * ownership states (owned Base Set holos at $100-$320, unowned SIRs at
 * $130-$280, plus sub-$1 commons of each), so the price order genuinely
 * interleaves them. Asserts (a) owned + unowned interleave in DOM order and
 * (b) the Adj. column is price-sorted — proof the two are one list.
 *
 * The sort itself became the server's in pd-tsqd (the page renders one bounded
 * page, so sorting it client-side would rank an arbitrary 250 rows), which
 * changed the granularity the order can be asserted at but not this test's
 * subject — see assertPriceSorted.
 */
import type { ReplayHarness } from '../replay';

interface Row {
  owned: boolean;
  /** Adj. column value, or null for a "—" cell. */
  price: number | null;
  /** The printing this row's copies belong to. */
  printing: string;
}

/** Read each table row's ownership + Adj. price (the second-to-last cell, so
 *  robust to a leading select-mode checkbox column), in DOM order. */
async function readTableRows(h: ReplayHarness): Promise<Row[]> {
  return h.page.locator('table.dd tbody tr').evaluateAll((els) =>
    els.map((el) => {
      const cell = el.querySelector('td:nth-last-child(2)'); // Adj. (Value is last)
      const txt = (cell?.textContent ?? '').trim();
      const num = Number(txt.replace(/[^0-9.]/g, ''));
      return {
        owned: !el.classList.contains('missing'),
        price: txt === '' || txt === '—' || Number.isNaN(num) ? null : num,
        printing: el.getAttribute('data-printing') ?? '',
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

/** The Adj. column must be globally ordered (asc or desc) — proof the sort
 *  engaged and spans owned + unowned together, not a per-block order.
 *
 *  Ordered AT PRINTING GRANULARITY, which is the granularity the sort has:
 *  the server orders printings (pd-tsqd — the page renders one bounded page,
 *  so a client-side sort would rank an arbitrary 250 rows), while the table
 *  draws one row per (printing, condition, status). A printing held Near Mint
 *  and Lightly Played is therefore two rows at two Adj. prices sitting in one
 *  slot of the global order, and the row-by-row sequence legitimately dips
 *  inside that slot. So compare each printing's LEADING row — and separately
 *  require the rows within a printing to be ordered too, which is the client's
 *  job precisely because the server cannot express it. */
function assertPriceSorted(rows: Row[], label: string): void {
  const prices = rows.map((r) => r.price);
  if (prices.some((p) => p === null)) {
    throw new Error(
      `${label}: some rows show no Adj. price ("—") — the fixture price join ` +
        `regressed (see pokedumpster-qm9). Prices: ${prices.join(', ')}`,
    );
  }
  const monotonic = (nums: number[]) => ({
    asc: nums.every((p, i) => i === 0 || nums[i - 1]! <= p),
    desc: nums.every((p, i) => i === 0 || nums[i - 1]! >= p),
  });

  // One entry per printing, in DOM order — its first row's Adj. price.
  const leads: number[] = [];
  const runs = new Map<string, number[]>();
  for (const r of rows) {
    const seen = runs.get(r.printing);
    if (seen) seen.push(r.price!);
    else {
      runs.set(r.printing, [r.price!]);
      leads.push(r.price!);
    }
  }
  const global = monotonic(leads);
  if (!global.asc && !global.desc) {
    throw new Error(
      `${label}: Adj. column not globally price-sorted across printings — ` +
        `owned + unowned are not one sorted list. Leads: ${leads.join(', ')}`,
    );
  }
  // Same direction inside each printing's run of copy-groups.
  for (const [printing, run] of runs) {
    if (run.length < 2) continue;
    const inner = monotonic(run);
    if (global.desc ? !inner.desc : !inner.asc) {
      throw new Error(
        `${label}: ${printing}'s copy-groups are not ordered by Adj. within ` +
          `the printing (${run.join(', ')}) — the client has to complete the ` +
          `order the per-printing server sort cannot express`,
      );
    }
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
    .locator('th.sortable', { hasText: 'Adj.' })
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
  // Sort state is shared with the table; click the grid Adj. button to
  // exercise the grid sort path too (direction may flip — interleave holds
  // either way). Grid tiles don't expose the price, so we assert ownership
  // interleave only here.
  await h.click_by_test_id('view-grid');
  await h.wait_for_visible('.cardgrid .cardtile.missing', 4000);
  await h.page
    .locator('.gridsort .sortbtn', { hasText: 'Adj.' })
    .first()
    .click({ timeout: 1000 });
  await h.page.waitForTimeout(100);
  assertInterleaved(await readGridOwnership(h), 'grid view');
  await h.screenshot('grid_sorted');
}
