// covers: UC-ui-085
// tier: T3
/**
 * Hand-written regression: a facet click inside the card modal stays
 * in-collection. The query alone decides results, so the link carries no
 * all=1 and the unowned dex-6 Charizard ex (sv3pt5/199) does NOT surface.
 */
import type { ReplayHarness } from '../replay';

const DEX = '[role="dialog"] a.facet[title="Find every card of Pokédex #6"]';

export async function steps(h: ReplayHarness) {
  await h.wait_for_visible('.cardtile', 6000);
  // Open the owned Charizard (BS #4) modal. "Charizard" sorts before
  // "Charizard ex", so the first match is base1/4.
  await h.click_by_selector('.cardtile[title^="Charizard"]');
  await h.wait_for_visible('[role="dialog"]', 6000);

  // Click the Pokédex #6 facet inside the modal — its href is an owned-only query.
  await h.wait_for_visible(DEX, 6000);
  await h.click_by_selector(DEX);
  await h.page.waitForURL(/\/collection\?q=pokedex/, { timeout: 6000 });
  if (h.page.url().includes('all=1')) throw new Error('facet nav must not carry all=1');

  // Stays owned-only: the owned Charizard is listed, no missing tile.
  await h.wait_for_visible('.cardtile', 6000);
  if ((await h.page.locator('.cardtile.missing').count()) > 0) {
    throw new Error('owned-only facet nav surfaced an unowned card');
  }
  await h.screenshot('final_state');
}
