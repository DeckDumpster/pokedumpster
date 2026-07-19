/**
 * Hand-written implementation for collectr_export_menu_links.
 *
 * Opens the collection overflow menu and asserts the two separate Collectr
 * export downloads (cards, sealed) are offered alongside the ManaBox export.
 */
import type { ReplayHarness } from '../replay';

export async function steps(h: ReplayHarness) {
  await h.navigate('/collection');
  // The header burger opens the overflow menu.
  await h.wait_for_visible('.burger');
  await h.click_by_selector('.burger');
  // Cards and sealed are offered as separate Collectr downloads.
  await h.wait_for_text('Export cards (Collectr)');
  await h.assert_text_present('Export cards (Collectr)');
  await h.assert_text_present('Export sealed (Collectr)');
  await h.screenshot('final_state');
}
