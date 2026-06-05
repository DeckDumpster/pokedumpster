/**
 * Hand-written implementation for collection_back_preserves_search.
 *
 * Regression guard for pokedumpster-k7v on /collection: typing a query
 * rewrites the URL to ?q=…; navigating to a card and pressing Back used to
 * drop the param (replaceState stashed the stale page.url as the restore
 * key). The fix uses goto(replaceState:true), so ?q= survives the round trip.
 */
import type { ReplayHarness } from '../replay';

export async function steps(h: ReplayHarness) {
  await h.navigate('/collection');
  await h.wait_for_visible('[data-testid="search-input"]');

  // Debounced, client-side URL rewrite of the active query.
  await h.fill_by_selector('[data-testid="search-input"]', 'char');
  await h.page.waitForURL(/[?&]q=char/, { timeout: 5000 });

  // Leave for a card detail page, then come back.
  await h.navigate('/card/sv8/191');
  await h.page.waitForURL(/\/card\//, { timeout: 5000 });
  await h.page.goBack({ waitUntil: 'networkidle' });

  // The query must still be in the URL and in the box.
  await h.page.waitForURL(/[?&]q=char/, { timeout: 5000 });
  const value = await h.page.locator('[data-testid="search-input"]').inputValue();
  if (value !== 'char') {
    throw new Error(`Back dropped the search query: input="${value}", url=${h.page.url()}`);
  }
  await h.screenshot('final_state');
}
