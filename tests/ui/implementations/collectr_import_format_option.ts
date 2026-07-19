/**
 * Hand-written implementation for collectr_import_format_option.
 *
 * Selects the Collectr format on the import page and asserts the
 * cards+sealed explanatory hint appears.
 */
import type { ReplayHarness } from '../replay';

export async function steps(h: ReplayHarness) {
  await h.navigate('/ingest/csv');
  await h.wait_for_text('Import CSV');
  // The format <select> is the only one on the page.
  await h.select_by_label('select', 'Collectr (cards + sealed)');
  // Choosing Collectr reveals a note about the split import.
  await h.wait_for_text('A Collectr export mixes single cards and sealed products');
  await h.assert_text_present('A Collectr export mixes single cards and sealed products');
  await h.screenshot('final_state');
}
