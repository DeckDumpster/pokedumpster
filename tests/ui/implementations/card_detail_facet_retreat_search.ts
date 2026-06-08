/**
 * Hand-written implementation for card_detail_facet_retreat_search.
 *
 * Guards pokedumpster-cww: the Retreat icons now link to retreat:<n>&all=1
 * (cards with the same retreat-cost count). Charizard (base1/4) retreats for
 * 3, as does Blastoise — so retreat:3 surfaces Blastoise.
 */
import type { ReplayHarness } from '../replay';

const RETREAT = 'a.retreat[title="Find all cards with a retreat cost of 3"]';

export async function steps(h: ReplayHarness) {
  await h.wait_for_visible(RETREAT, 6000);
  await h.click_by_selector(RETREAT);
  await h.page.waitForURL(/\/collection\?/, { timeout: 6000 });
  const url = h.page.url();
  if (!url.includes('q=retreat')) throw new Error(`expected a retreat: DSL query, got: ${url}`);
  if (!url.includes('all=1')) throw new Error(`expected catalog-wide all=1, got: ${url}`);
  await h.wait_for_visible('.cardtile[title^="Blastoise"]', 6000);
  await h.screenshot('final_state');
}
