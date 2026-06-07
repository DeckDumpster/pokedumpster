/**
 * Hand-written implementation for collection_add_from_modal.
 *
 * Clicking a collection tile opens the card modal (CardModal → CardDetailView).
 * Its Printings table has a "+" button per variant; clicking it adds a copy
 * with source "manual" and re-runs load(), so the "Your copies (N)" header
 * increments. Blastoise (base1 #2) is owned with a single copy in the fixture.
 *
 * Note: the original hint describes a pre-modal "Name link → /card page" flow.
 * The card detail now renders inside the collection modal; the add mutation and
 * its "Your copies" feedback are identical, so this exercises the same intent.
 */
import type { ReplayHarness } from '../replay';

export async function steps(h: ReplayHarness) {
  await h.wait_for_visible('.cardtile', 4000);
  // Open the owned Blastoise tile — its modal shows one copy.
  await h.click_by_selector('.cardtile[title^="Blastoise"]');
  await h.wait_for_visible('[role="dialog"]', 4000);
  await h.assert_text_present('Your copies (1)');
  // "+ Add" on the variant row adds a manual copy (non-optimistic reload).
  await h.click_by_selector('[role="dialog"] button[aria-label^="Add one"]');
  await h.wait_for_text('Your copies (2)', 4000);
  await h.assert_text_present('Your copies (2)');
  await h.screenshot('final_state');
}
