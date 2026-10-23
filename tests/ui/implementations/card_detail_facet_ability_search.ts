// covers: UC-ui-065
// tier: T3
/**
 * Hand-written implementation for card_detail_facet_ability_search.
 *
 * Guards pokedumpster-cww: a Pokémon's ability name now links to
 * ability:"<name>". Blastoise (base1/2) has Rain Dance in the fixture;
 * the search returns Blastoise.
 */
import type { ReplayHarness } from '../replay';

const ABILITY = 'a.abilityName';

export async function steps(h: ReplayHarness) {
  await h.wait_for_visible(ABILITY, 6000);
  await h.assert_text_present('Abilities');
  await h.click_by_selector(ABILITY);
  await h.page.waitForURL(/\/collection\?/, { timeout: 6000 });
  const url = h.page.url();
  if (!url.includes('q=ability')) throw new Error(`expected an ability: DSL query, got: ${url}`);
  if (new URL(url).searchParams.has('all')) throw new Error(`facet link must not carry an all param, got: ${url}`);
  await h.wait_for_visible('.cardtile[title^="Blastoise"]', 6000);
  await h.screenshot('final_state');
}
