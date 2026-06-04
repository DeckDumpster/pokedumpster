/**
 * Hand-written implementation for collection_search_unowned.
 *
 * With "All cards" on, a t:water query surfaces unowned printings as dimmed
 * "missing" grid tiles alongside owned ones.
 */
import type { ReplayHarness } from '../replay';

export async function steps(h: ReplayHarness) {
  await h.navigate('/collection');
  await h.wait_for_visible('[data-testid="search-input"]');
  // Grid view renders the dimmed .missing tiles for unowned printings.
  await h.click_by_test_id('view-grid');
  // Widen the search to the whole catalog (include_unowned=1).
  await h.click_by_test_id('all-cards-toggle');
  await h.fill_by_selector('[data-testid="search-input"]', 't:water');
  // At least one unowned (missing) tile appears.
  await h.wait_for_visible('.cardtile.missing');
  await h.assert_visible('.cardtile.missing');
  await h.screenshot('final_state');
}
