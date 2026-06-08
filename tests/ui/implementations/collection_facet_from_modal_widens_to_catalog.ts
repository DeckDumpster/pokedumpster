/**
 * Hand-written fail-first regression for the "all=1 ignored on client-side
 * facet nav" bug: clicking a facet link inside the card modal navigated to
 * /collection?q=...&all=1 but the page only resynced the `q` param from the
 * URL, not `all` — so the catalog stayed owned-only until a manual reload.
 *
 * Charizard (base1/4) is owned and shares Pokédex #6 with the unowned
 * Charizard ex (sv3pt5/199). Clicking the Pokédex #6 facet in the modal must
 * immediately surface that unowned card as a dimmed .cardtile.missing tile,
 * with no reload. Fails before the +page.svelte resync fix.
 */
import type { ReplayHarness } from '../replay';

const DEX = '[role="dialog"] a.facet[title="Find every card of Pokédex #6"]';

export async function steps(h: ReplayHarness) {
  await h.wait_for_visible('.cardtile', 6000);
  // Open the owned Charizard (BS #4) modal. "Charizard" sorts before
  // "Charizard ex", so the first match is base1/4.
  await h.click_by_selector('.cardtile[title^="Charizard"]');
  await h.wait_for_visible('[role="dialog"]', 6000);

  // Click the Pokédex #6 facet inside the modal — its href carries &all=1.
  await h.wait_for_visible(DEX, 6000);
  await h.click_by_selector(DEX);
  await h.page.waitForURL(/\/collection\?.*all=1/, { timeout: 6000 });

  // The catalog must widen on this client-side nav (no reload): the unowned
  // dex-6 Charizard ex appears as a dimmed missing tile.
  await h.wait_for_visible('.cardtile.missing', 6000);
  await h.assert_visible('.cardtile.missing');
  await h.screenshot('final_state');
}
