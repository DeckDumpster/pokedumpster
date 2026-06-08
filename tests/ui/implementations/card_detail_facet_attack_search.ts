/**
 * Hand-written implementation for card_detail_facet_attack_search.
 *
 * Guards pokedumpster-cww: a Pokémon's attack name now links to
 * attack:"<name>"&all=1. Charizard (base1/4) has the Fire Spin attack; the
 * search returns Charizard.
 */
import type { ReplayHarness } from '../replay';

const ATTACK = 'a.attackName';

export async function steps(h: ReplayHarness) {
  await h.wait_for_visible(ATTACK, 6000);
  await h.assert_text_present('Attacks');
  await h.click_by_selector(ATTACK);
  await h.page.waitForURL(/\/collection\?/, { timeout: 6000 });
  const url = h.page.url();
  if (!url.includes('q=attack')) throw new Error(`expected an attack: DSL query, got: ${url}`);
  if (!url.includes('all=1')) throw new Error(`expected catalog-wide all=1, got: ${url}`);
  await h.wait_for_visible('.cardtile[title^="Charizard"]', 6000);
  await h.screenshot('final_state');
}
