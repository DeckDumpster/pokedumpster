/**
 * Hand-written implementation for collection_search_keyword_filter.
 *
 * Types a `t:fire` query and asserts the server-side filter narrows the
 * table to Fire cards (Charizard present, Lightning Pikachu absent).
 */
import type { ReplayHarness } from '../replay';

export async function steps(h: ReplayHarness) {
  await h.navigate('/collection');
  await h.wait_for_visible('[data-testid="search-input"]');
  // Table view renders card names as text, which we can assert on.
  await h.click_by_test_id('view-table');
  // Debounced, server-side query.
  await h.fill_by_selector('[data-testid="search-input"]', 't:fire');
  await h.wait_for_text('Charizard');
  await h.assert_text_present('Charizard');
  // Pikachu is a Lightning card — excluded by t:fire.
  await h.assert_text_absent('Pikachu');
  await h.screenshot('final_state');
}
