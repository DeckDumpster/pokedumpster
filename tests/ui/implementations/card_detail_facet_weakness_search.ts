/**
 * Hand-written implementation for card_detail_facet_weakness_search.
 *
 * Guards pokedumpster-cww: the Weakness icon now links to weakness:<T>&all=1
 * (cards sharing that type weakness). Charizard (base1/4) is weak to Water,
 * as are the fixture's other Fire cards — so Charmander appears too.
 */
import type { ReplayHarness } from '../replay';

const WEAK = 'a.wr[title="Find all cards weak to Water"]';

export async function steps(h: ReplayHarness) {
  await h.wait_for_visible(WEAK, 6000);
  await h.click_by_selector(WEAK);
  await h.page.waitForURL(/\/collection\?/, { timeout: 6000 });
  const url = h.page.url();
  if (!url.includes('q=weakness')) throw new Error(`expected a weakness: DSL query, got: ${url}`);
  if (!url.includes('all=1')) throw new Error(`expected catalog-wide all=1, got: ${url}`);
  await h.wait_for_visible('.cardtile[title^="Charmander"]', 6000);
  await h.screenshot('final_state');
}
