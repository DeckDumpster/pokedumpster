/**
 * Hand-written implementation for collection_search_help_page.
 *
 * The "?" link beside the search bar opens the data-driven /search-help
 * syntax reference.
 */
import type { ReplayHarness } from '../replay';

export async function steps(h: ReplayHarness) {
  await h.navigate('/collection');
  await h.wait_for_visible('[data-testid="search-help-link"]');
  await h.click_by_test_id('search-help-link');
  await h.wait_for_visible('[data-testid="search-help"]');
  await h.assert_text_present('Search syntax');
  // A keyword rendered from the server registry.
  await h.assert_text_present('Energy type');
  await h.screenshot('final_state');
}
