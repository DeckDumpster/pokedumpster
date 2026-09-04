/**
 * Hand-written implementation for browse_japanese_set_tile_not_provisional.
 *
 * The Japanese catalog is TCGCSV-native, not provisional (pd-mt57): a `jp-`
 * set has a NULL `ptcgio_fetched_at` exactly like an English set upstream
 * has not published yet, and `ptcgio_covered` is the only thing that tells
 * the two apart. So the absent "TCGCSV" badge IS the assertion here.
 */
import type { ReplayHarness } from '../replay';

const TILE = 'a.tile[href="/browse/jp-23723"]';
const HEADER = 'button.grouphdr:has-text("Pokémon JP — Original Era")';

export async function steps(h: ReplayHarness) {
  await h.navigate('/browse');
  await h.wait_for_visible('.browsepage');

  // The JP era is the one series in the fixture nothing is owned in, so it
  // is the one series that defaults to collapsed — its tile is not rendered.
  await h.wait_for_visible(HEADER);
  await h.assert_hidden(TILE);

  await h.click_by_selector(HEADER);
  await h.wait_for_visible(TILE);

  // No symbol art, and no printed code either, so the tile stamps the
  // catalogue key. Nothing else in the fixture takes this fallback.
  await h.assert_visible(`${TILE} span.symbol span.code`);
  await h.assert_text_present('JP-23723');

  // The badge that must NOT be here. Scoped to the tile rather than to the
  // page: a synthesized ENGLISH set is legitimately badged, and one added to
  // the fixture later must not read as this set regressing.
  await h.assert_hidden(`${TILE} .tags`);

  // printed_total is NULL, so `base_total_cards` comes back null and the
  // Base meter is dropped — one bar, not two.
  await h.assert_element_count(`${TILE} .bar`, 1);
  await h.assert_visible(`${TILE} [aria-label="Master 0 / 6"]`);

  await h.screenshot('final_state');
}
