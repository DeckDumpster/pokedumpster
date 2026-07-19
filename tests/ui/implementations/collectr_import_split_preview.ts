/**
 * Hand-written implementation for collectr_import_split_preview.
 *
 * Pastes a mixed Collectr export and asserts the preview renders three
 * separate panes — Single cards, Sealed products, Skipped — proving the
 * garden wall holds in the UI. Assertions target the preview's own DOM
 * (the `.pane-head` sections and backend-generated text like the resolved
 * set code and the skip reason) rather than text that also appears in the
 * Collectr hint paragraph or the pasted CSV.
 */
import type { ReplayHarness } from '../replay';

// 6 data rows: Lorcana (skipped); Base Set Charizard (matched); Base Set
// Bulbasaur qty 2 (matched, 2 copies); Base Set Fakemon (unmatched card);
// 151 ETB (matched sealed); Surging Sparks pack qty 6 (matched sealed);
// Nonexistent Box (unmatched sealed).
const CSV = `Portfolio Name,Category,Set,Product Name,Card Number,Rarity,Variance,Grade,Card Condition,Average Cost Paid,Quantity,Market Price (As of 2026-07-19),Price Override,Watchlist,Date Added,Notes
Main,Lorcana,Disney Lorcana Promo Cards,Elsa - The Fifth Spirit,6,Promo,Holofoil,Ungraded,Near Mint,0,1,7.12,0,false,2026-01-24,
Main,Pokemon,Base Set,Charizard,4/102,Rare Holo,Holofoil,Ungraded,Lightly Played,280,1,300,0,false,2024-01-10,
Main,Pokemon,Base Set,Bulbasaur,46/102,Common,Normal,Ungraded,Near Mint,1.00,2,2,0,false,2024-01-10,
Main,Pokemon,Base Set,Fakemon,999/102,Common,Normal,Ungraded,Near Mint,0,1,0,0,false,2024-01-10,
Sealed Pokemon TCG,Pokemon,151,151 Elite Trainer Box,,,Normal,Ungraded,Near Mint,49.99,1,60,0,false,2023-09-22,
Sealed Pokemon TCG,Pokemon,Surging Sparks,Surging Sparks Booster Pack,,,Normal,Ungraded,Near Mint,4.49,6,5,0,false,2024-11-08,
Sealed Pokemon TCG,Pokemon,151,Nonexistent Box,,,Normal,Ungraded,Near Mint,10,1,10,0,false,2024-01-01,`;

export async function steps(h: ReplayHarness) {
  await h.navigate('/ingest/csv');
  await h.wait_for_text('Import CSV', 8000);
  await h.select_by_label('select', 'Collectr (cards + sealed)');
  await h.fill_by_selector('textarea', CSV);
  await h.click_by_text('Preview');

  // The preview renders three distinct panes (Single cards, Sealed products,
  // Skipped) — `.pane-head` exists only in the preview, not the hint.
  await h.wait_for_visible('.pane-head', 8000);
  await h.assert_element_count('.pane-head', 3);

  // A matched card resolved to its catalog set code (`base1`) — this text is
  // produced by the backend, so it only appears in the Single cards pane.
  await h.assert_text_present('base1');
  // Each split pane surfaced its own unmatched sub-table.
  await h.assert_text_present('Unmatched cards');
  await h.assert_text_present('Unmatched sealed');
  // The non-Pokémon row is reported skipped with a backend-generated reason.
  await h.assert_text_present("non-Pokémon category");
  await h.screenshot('final_state');
}
