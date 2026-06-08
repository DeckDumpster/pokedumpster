/**
 * Hand-written implementation for collection_table_column_facets.
 *
 * Guards pokedumpster-ozm: in the collection table view, the whole row no
 * longer opens the modal. Only the Name cell (td.namecol) opens the card;
 * every other column runs a DSL facet search. We click the Set cell (a
 * constant-title facet cell) and assert a set: search ran with NO modal,
 * then click a Name cell and assert the modal opens.
 */
import type { ReplayHarness } from '../replay';

const SET_CELL = 'table.dd tbody td.fac[title="Find all cards in this set"]';

export async function steps(h: ReplayHarness) {
  await h.wait_for_visible('[data-testid="search-input"]', 6000);
  await h.click_by_test_id('view-table');
  await h.wait_for_visible('table.dd tbody tr', 6000);

  // ── Set column → facet search, not a modal ──────────────────────────
  await h.wait_for_visible(SET_CELL, 6000);
  await h.click_by_selector(SET_CELL);
  await h.page.waitForURL(/\/collection\?/, { timeout: 6000 });
  const url = h.page.url();
  if (!url.includes('q=set')) throw new Error(`expected a set: DSL query, got: ${url}`);
  if (!url.includes('all=1')) throw new Error(`expected catalog-wide all=1, got: ${url}`);
  if ((await h.page.locator('[role="dialog"]').count()) > 0) {
    throw new Error('clicking a non-Name column wrongly opened the card modal');
  }
  await h.screenshot('set_facet_search');

  // ── Name column → opens the card modal ──────────────────────────────
  await h.wait_for_visible('table.dd tbody td.namecol', 6000);
  await h.click_by_selector('table.dd tbody td.namecol');
  await h.wait_for_visible('[role="dialog"]', 6000);
  await h.assert_visible('[role="dialog"]');
  await h.screenshot('final_state');
}
