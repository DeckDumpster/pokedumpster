// covers: UC-ui-070
// tier: T3
/**
 * Hand-written implementation for card_detail_facet_type_search.
 *
 * Guards pokedumpster-xj6: the Element/type link now emits type:<T>
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
  if (new URL(url).searchParams.has('all')) throw new Error(`facet link must not carry an all param, got: ${url}`);
  await h.wait_for_visible('.cardtile[title^="Charmander"]', 6000);
  await h.screenshot('final_state');
}
