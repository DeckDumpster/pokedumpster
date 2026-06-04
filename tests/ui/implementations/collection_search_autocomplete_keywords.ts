/**
 * Hand-written implementation for collection_search_autocomplete_keywords.
 *
 * Typing a bare token suggests keywords; typing "is:" suggests flag values.
 */
import type { ReplayHarness } from '../replay';

export async function steps(h: ReplayHarness) {
  await h.navigate('/collection');
  await h.wait_for_visible('[data-testid="search-input"]');
  await h.click_by_test_id('search-input');
  // Bare token -> keyword suggestions.
  await h.fill_by_selector('[data-testid="search-input"]', 't');
  await h.wait_for_visible('[data-testid="search-autocomplete"]');
  await h.assert_text_present('Energy type');
  // "is:" -> flag value suggestions.
  await h.fill_by_selector('[data-testid="search-input"]', 'is:');
  await h.wait_for_visible('[data-testid="search-autocomplete"]');
  await h.assert_text_present('is:holo');
  await h.screenshot('final_state');
}
