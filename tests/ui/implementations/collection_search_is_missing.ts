/**
 * Hand-written implementation for collection_search_is_missing.
 *
 * Regression guard for pokedumpster-f5j: "is:missing" returns every unowned
 * printing, and the fixture has several cards with two unowned printings each,
 * so the result carries duplicate card_ids. The missing tiles must key on the
 * unique printing_id — keyed on card_id, the {#each} threw each_key_duplicate
 * and the grid rendered nothing (the page hung on "Loading…").
 */
import type { ReplayHarness } from '../replay';

export async function steps(h: ReplayHarness) {
  await h.wait_for_visible('[data-testid="search-input"]');
  // Grid view renders the dimmed .missing tiles for unowned printings.
  await h.click_by_test_id('view-grid');
  await h.fill_by_selector('[data-testid="search-input"]', 'is:missing');
  // The grid fills with dimmed missing tiles. If the keyed {#each} collided
  // on a duplicate card_id, zero tiles render and this wait times out.
  await h.wait_for_visible('.cardtile.missing', 4000);
  await h.assert_visible('.cardtile.missing');
  await h.screenshot('final_state');
}
