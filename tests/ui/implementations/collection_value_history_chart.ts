/**
 * Hand-written implementation for collection_value_history_chart.
 *
 * The Collection header's "{N} cards, {$X}" total is a button that opens the
 * collection-value-over-time dialog (pd-0a30f178). This walks the entry point:
 * click the total, the dialog mounts and fetches
 * /api/collection/value-history?dimension=all, and the chart container renders
 * with one series. Escape closes it.
 */
import type { ReplayHarness } from '../replay';

const DIALOG = '[role="dialog"][aria-label="Collection value over time"]';
const CHART = '[data-testid="value-history-chart"]';

export async function steps(h: ReplayHarness) {
  // The grid has to be up before the header total carries its value.
  await h.wait_for_visible('.cardtile', 6000);

  // The header total is the entry point — it must be a real button.
  await h.assert_visible('button.countline');
  await h.click_by_selector('button.countline');

  // The dialog fetches its series on mount, so wait for the chart, not
  // just the dialog frame.
  await h.wait_for_visible(DIALOG, 6000);
  await h.wait_for_visible(CHART, 6000);
  await h.assert_text_present('Collection value over time');

  // The 'all' dimension answers with the collection's priced halves — the
  // loose cards (bucket = NULL) and, for a tenant who owns sealed product, a
  // second series (pd-bbv7). The committed fixture holds no sealed lots, so
  // one line here is the whole answer rather than a rule about the endpoint.
  await h.assert_element_count(`${CHART}[data-series-count="1"]`, 1);

  // Escape returns me to the collection.
  await h.press_key('Escape');
  await h.wait_for_hidden(DIALOG, 6000);

  await h.screenshot('final_state');
}
