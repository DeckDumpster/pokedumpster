/**
 * Hand-written implementation for card_detail_evolves_into_search.
 *
 * Guards pokedumpster-anj: the label reads "Evolves into" (grammar fix, was
 * "Evolves to"), and the link runs name:"<evolution>"&all=1. Pikachu
 * (base1/58) evolves into Raichu (base1/24) in the fixture.
 */
import type { ReplayHarness } from '../replay';

export async function steps(h: ReplayHarness) {
  await h.wait_for_visible('a.evolink', 6000);
  // Grammar fix: the label must read "Evolves into", not "Evolves to".
  await h.assert_text_present('Evolves into');
  await h.click_by_selector('a.evolink');
  await h.page.waitForURL(/\/collection\?/, { timeout: 6000 });
  const url = h.page.url();
  if (!url.includes('q=name')) throw new Error(`expected a name: DSL query, got: ${url}`);
  if (!url.includes('all=1')) throw new Error(`expected catalog-wide all=1, got: ${url}`);
  await h.wait_for_visible('.cardtile[title^="Raichu"]', 6000);
  await h.screenshot('final_state');
}
