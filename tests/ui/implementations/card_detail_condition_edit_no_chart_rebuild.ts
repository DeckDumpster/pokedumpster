/**
 * Hand-written fail-first regression for the "price chart refreshes on every
 * copy edit" bug (pokedumpster-i5d). Editing a copy's condition calls load(),
 * which re-fetches the (unchanged) catalog price history and hands PriceChart
 * a new-but-identical series array. The old PriceChart destroyed + recreated
 * the Chart.js chart on any new array reference, replaying the entry animation
 * — a jarring visual "refresh". The fix rebuilds only when the plotted data's
 * content signature changes; data-builds counts (re)builds for the assertion.
 */
import type { ReplayHarness } from '../replay';

const CHART = '[data-testid="price-chart"]';
const CONDITION = 'td[data-label="Condition"] select';

export async function steps(h: ReplayHarness) {
  await h.wait_for_visible(CHART, 6000);
  await h.wait_for_visible(CONDITION, 6000);

  const before = await h.page.getByTestId('price-chart').first().getAttribute('data-builds');
  if (before === null) throw new Error('price chart did not render a data-builds counter');

  // Change the copy's condition (Near Mint -> Lightly Played).
  await h.select_by_label(CONDITION, 'Lightly Played');
  // The inline ✓ confirms the edit committed (fires after the reload).
  await h.wait_for_visible('.cellSaved', 5000);

  const after = await h.page.getByTestId('price-chart').first().getAttribute('data-builds');
  if (after !== before) {
    throw new Error(
      `price chart rebuilt on a condition edit (data-builds ${before} -> ${after}); ` +
        `the chart must not refresh when the price data is unchanged`,
    );
  }
  await h.screenshot('final_state');
}
