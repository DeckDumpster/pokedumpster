/**
 * Hand-written implementation for card_detail_facet_rarity_search.
 *
 * Guards pokedumpster-xj6: the rarity link now emits rarity:"<R>"&all=1
 * instead of a bare name search. Charizard (base1/4) is Rare Holo, as are
 * base1 Blastoise and Venusaur — so the search returns several, Blastoise
 * among them.
 */
import type { ReplayHarness } from '../replay';

const RARITY = 'a.facet[title="Find all Rare Holo cards"]';

export async function steps(h: ReplayHarness) {
  await h.wait_for_visible(RARITY, 6000);
  await h.click_by_selector(RARITY);
  await h.page.waitForURL(/\/collection\?/, { timeout: 6000 });
  const url = h.page.url();
  if (!url.includes('q=rarity')) throw new Error(`expected a rarity: DSL query, got: ${url}`);
  if (!url.includes('all=1')) throw new Error(`expected catalog-wide all=1, got: ${url}`);
  await h.wait_for_visible('.cardtile[title^="Blastoise"]', 6000);
  await h.screenshot('final_state');
}
