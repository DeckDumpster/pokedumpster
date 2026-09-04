/**
 * Hand-written implementation for browse_japanese_set_add_first_copy.
 *
 * The whole walk over the one Japanese set in the fixture: /browse →
 * binder page → variant modal → card detail → add a copy. Everything it
 * asserts along the way is a path the three English sets cannot reach — a
 * NULL printed_total, a synthetic `p<product_id>` collector number, a card
 * TCGCSV publishes no art for, and the vintage 1st Edition / Unlimited
 * print runs (pd-zonm).
 */
import type { ReplayHarness } from '../replay';

const TILE = 'a.tile[href="/browse/jp-23723"]';
const HEADER = 'button.grouphdr:has-text("Pokémon JP — Original Era")';

export async function steps(h: ReplayHarness) {
  await h.navigate('/browse');
  await h.wait_for_visible(HEADER);
  await h.click_by_selector(HEADER);
  await h.click_by_selector(TILE);

  // --- binder page -------------------------------------------------------
  await h.wait_for_visible('.binderpage .grid');
  // This route has no h1; the breadcrumb is where the set names itself.
  await h.assert_text_present('Mystery of the Fossils');
  // printed_total is NULL, so `section_of` files EVERY card as base — with a
  // numeric printed_total these synthetic numbers sort at 1,475,661 and would
  // every one of them read as a secret rare. Base counts cards (6); Master
  // counts printings, and this set carries two print runs per card (12).
  await h.assert_visible('[aria-label="Base 0/6"]');
  await h.assert_visible('[aria-label="Master 0/12"]');
  await h.assert_text_present('Page 1 of 1');
  await h.assert_element_count('.grid .slotwrap', 6);
  await h.assert_element_count('.grid .slotwrap.missing', 6);
  // TCGCSV reports imageCount 0 for Lapras, so that slot draws its name.
  await h.assert_element_count('.grid .noart', 1);

  // The default binder view draws no name plate, so the image alt is how a
  // slot is addressed. (The in-set search is debounced — not a test seam.)
  await h.click_by_selector('.slotwrap:has(img[alt="Ekans"]) button.slot');
  await h.wait_for_visible('a.full');
  await h.click_by_selector('a.full');

  // --- card detail -------------------------------------------------------
  await h.wait_for_visible('ul.printings');
  // The synthetic collector number, reaching the UI.
  await h.assert_text_present('Ekans #p575663');
  await h.assert_text_present('Your copies (0)');
  await h.assert_text_present("You don't own this card yet.");
  // The vintage print runs, which exist nowhere else in the fixture.
  await h.assert_text_present('1st Edition Normal');
  await h.assert_text_present('Unlimited Normal');

  await h.click_by_selector('button.step[aria-label="Add one 1st Edition Normal"]');
  // The add is not optimistic: the page re-runs its load first.
  await h.wait_for_text('Your copies (1)');
  await h.assert_text_present('Your copies (1)');

  await h.screenshot('final_state');
}
