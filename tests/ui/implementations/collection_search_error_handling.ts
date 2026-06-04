/**
 * Hand-written implementation for collection_search_error_handling.
 *
 * An unknown keyword surfaces an inline parse error (message + position);
 * clearing the box removes it.
 */
import type { ReplayHarness } from '../replay';

export async function steps(h: ReplayHarness) {
  await h.navigate('/collection');
  await h.wait_for_visible('[data-testid="search-input"]');
  // Unknown keyword -> server 400 -> inline error.
  await h.fill_by_selector('[data-testid="search-input"]', 'xyz:1');
  await h.wait_for_visible('[data-testid="search-error"]');
  await h.assert_text_present('unknown keyword');
  await h.assert_text_present('position');
  // Clearing the query removes the error.
  await h.fill_by_selector('[data-testid="search-input"]', '');
  await h.wait_for_hidden('[data-testid="search-error"]');
  await h.screenshot('final_state');
}
