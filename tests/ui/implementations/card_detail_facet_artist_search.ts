/**
 * Hand-written implementation for card_detail_facet_artist_search.
 *
 * Guards pokedumpster-xj6: card-page facet links once emitted
 * /collection?q=<value> — a bare card-NAME search — so clicking the artist
 * "Mitsuhiro Arita" found nothing (no card is NAMED that). The shared
 * facetHref now emits artist:"<name>"&all=1 (catalog-wide). Charizard
 * (base1/4) is by Mitsuhiro Arita, who also drew base1 Venusaur/Bulbasaur/
 * Pikachu — so the search returns several cards, Venusaur among them.
 */
import type { ReplayHarness } from '../replay';

const ARTIST = 'a.facet[title="Find all cards by Mitsuhiro Arita"]';

export async function steps(h: ReplayHarness) {
  await h.wait_for_visible(ARTIST, 6000);
  await h.click_by_selector(ARTIST);
  await h.page.waitForURL(/\/collection\?/, { timeout: 6000 });
  const url = h.page.url();
  if (!url.includes('q=artist')) throw new Error(`expected an artist: DSL query, got: ${url}`);
  if (!url.includes('all=1')) throw new Error(`expected catalog-wide all=1, got: ${url}`);
  // A bare name search for the artist returns 0; the artist facet returns the
  // artist's catalog. Venusaur (a different card by the same artist) proves it.
  await h.wait_for_visible('.cardtile[title^="Venusaur"]', 6000);
  await h.assert_visible('.cardtile[title^="Charizard"]');
  await h.screenshot('final_state');
}
