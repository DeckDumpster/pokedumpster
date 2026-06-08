/**
 * Hand-written implementation for card_detail_pokedex_search.
 *
 * Guards pokedumpster-owh: the card page shows the National Pokédex number,
 * and clicking it emits pokedex:<n>&all=1. Mew (sv3pt5/131) is #151, shared
 * by Mew ex (sv3pt5/201) — so pokedex:151 surfaces Mew ex too, proving it
 * matched by Pokédex number rather than the card name.
 */
import type { ReplayHarness } from '../replay';

const DEX = 'a.facet[title="Find every card of Pokédex #151"]';

export async function steps(h: ReplayHarness) {
  await h.wait_for_visible(DEX, 6000);
  await h.click_by_selector(DEX);
  await h.page.waitForURL(/\/collection\?/, { timeout: 6000 });
  const url = h.page.url();
  if (!url.includes('q=pokedex')) throw new Error(`expected a pokedex: DSL query, got: ${url}`);
  if (!url.includes('all=1')) throw new Error(`expected catalog-wide all=1, got: ${url}`);
  // The OTHER #151 card — proof it matched by dex number, not name.
  await h.wait_for_visible('.cardtile[title^="Mew ex"]', 6000);
  await h.screenshot('final_state');
}
