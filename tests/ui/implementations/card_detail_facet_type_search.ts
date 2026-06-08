/**
 * Hand-written implementation for card_detail_facet_type_search.
 *
 * Guards pokedumpster-xj6: the Element/type link now emits type:<T>&all=1
 * (energy_type DSL) instead of a bare name search. Charizard (base1/4) is
 * Fire; the search surfaces other Fire cards, e.g. Charmander.
 */
import type { ReplayHarness } from '../replay';

const TYPE = 'a.facet[title="Find all Fire cards"]';

export async function steps(h: ReplayHarness) {
  await h.wait_for_visible(TYPE, 6000);
  await h.click_by_selector(TYPE);
  await h.page.waitForURL(/\/collection\?/, { timeout: 6000 });
  const url = h.page.url();
  if (!url.includes('q=type')) throw new Error(`expected a type: DSL query, got: ${url}`);
  if (!url.includes('all=1')) throw new Error(`expected catalog-wide all=1, got: ${url}`);
  await h.wait_for_visible('.cardtile[title^="Charmander"]', 6000);
  await h.screenshot('final_state');
}
