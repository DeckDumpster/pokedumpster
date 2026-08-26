/**
 * Hand-written implementation for collection_value_history_breakdown_tabs.
 *
 * The value dialog's three tabs each refetch a different dimension of
 * /api/collection/value-history (pd-0a30f178). The fixture spans three sets
 * and three binders, so switching tabs must take the chart from one line
 * ('all') to three ('set', then 'binder').
 *
 * The assertion hangs off the chart container's `data-series-count`, not the
 * legend: Chart.js paints the legend into the canvas, so set and binder names
 * are pixels, not DOM text.
 */
import type { ReplayHarness } from '../replay';

const DIALOG = '[role="dialog"][aria-label="Collection value over time"]';
const CHART = '[data-testid="value-history-chart"]';

/** The tab button carrying `label` inside the value dialog. */
function tab(label: string): string {
  return `${DIALOG} [role="tab"]:has-text("${label}")`;
}

/** Wait for the chart to settle on `n` plotted series after a tab switch. */
async function expectSeries(h: ReplayHarness, n: number) {
  await h.wait_for_visible(`${CHART}[data-series-count="${n}"]`, 6000);
}

export async function steps(h: ReplayHarness) {
  await h.wait_for_visible('.cardtile', 6000);
  await h.click_by_selector('button.countline');
  await h.wait_for_visible(DIALOG, 6000);

  // Opens on Total: the loose cards as one line. A fixture holding sealed
  // product would make this two (pd-bbv7) — the committed one holds none.
  await h.assert_visible(`${tab('Total')}[aria-selected="true"]`);
  await expectSeries(h, 1);

  // By set — one line per set the collection touches.
  await h.click_by_selector(tab('By set'));
  await h.assert_visible(`${tab('By set')}[aria-selected="true"]`);
  await expectSeries(h, 3);

  // By binder — one line per binder holding cards.
  await h.click_by_selector(tab('By binder'));
  await h.assert_visible(`${tab('By binder')}[aria-selected="true"]`);
  await expectSeries(h, 3);

  await h.screenshot('final_state');
}
