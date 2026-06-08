/**
 * Hand-written implementation for card_detail_evolves_from_search.
 *
 * Guards pokedumpster-anj: the "Evolves from" link no longer resolves to one
 * arbitrary newest printing — it now runs name:"<pre-evolution>"&all=1,
 * listing every printing of it (how evolution works, by name across sets).
 * Raichu (base1/24) evolves from Pikachu; the fixture has Pikachu in base1
 * and sv3pt5.
 */
import type { ReplayHarness } from '../replay';

export async function steps(h: ReplayHarness) {
  await h.wait_for_visible('a.evolink', 6000);
  await h.assert_text_present('Evolves from');
  await h.click_by_selector('a.evolink');
  await h.page.waitForURL(/\/collection\?/, { timeout: 6000 });
  const url = h.page.url();
  if (!url.includes('q=name')) throw new Error(`expected a name: DSL query, got: ${url}`);
  if (!url.includes('all=1')) throw new Error(`expected catalog-wide all=1, got: ${url}`);
  await h.wait_for_visible('.cardtile[title^="Pikachu"]', 6000);
  await h.screenshot('final_state');
}
