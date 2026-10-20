// covers: UC-ui-230
// tier: T3
/**
 * Hand-written implementation for card_detail_shows_copy_acquired_date.
 * Blastoise (base1/2) has one copy acquired 2024-01-10 in the fixture. The
 * date must appear in the copy's Acquired cell on the full card page and in
 * the card modal (same CardDetailView) opened from the Collection page.
 */
import type { ReplayHarness } from '../replay';

const CELL = 'td[data-label="Acquired"]';

export async function steps(h: ReplayHarness) {
  // Full card page (the scenario's start_page).
  await h.wait_for_visible(CELL, 6000);
  await h.page.waitForFunction(
    (sel) => document.querySelector(sel)?.textContent?.trim() === '2024-01-10',
    CELL,
    { timeout: 4000 },
  );
  await h.screenshot('card_page');

  // Card modal.
  await h.navigate('/collection');
  await h.wait_for_visible('.cardtile', 4000);
  await h.click_by_selector('.cardtile[title^="Blastoise"]');
  await h.wait_for_visible(`[role="dialog"] ${CELL}`, 4000);
  await h.page.waitForFunction(
    (sel) =>
      document.querySelector(`[role="dialog"] ${sel}`)?.textContent?.trim() === '2024-01-10',
    CELL,
    { timeout: 4000 },
  );
  await h.screenshot('final_state');
}
