// covers: UC-ui-229
// tier: T3
/**
 * Hand-written implementation for set_value_top_cards_missing_only.
 *
 * Rows the viewer owns carry `.owned`. Whatever the full list holds, the
 * missing-only list must hold none of them, and the toggle must be remembered
 * in the URL.
 */
import type { ReplayHarness } from '../replay';

const ROWS = '[data-testid="top-cards"] .toprow';

export async function steps(h: ReplayHarness) {
  await h.navigate('/browse/sv3pt5/stats');
  await h.wait_for_visible(ROWS, 6000);

  const before = await h.page.locator(ROWS).allInnerTexts();
  if (before.length !== 10) throw new Error(`expected 10 rows, got ${before.length}`);

  await h.page.getByLabel('Missing only').check();
  await h.page.waitForFunction(
    (sel) => document.querySelectorAll(sel + '.owned').length === 0,
    ROWS,
    { timeout: 4000 }
  );
  await h.page.waitForFunction(() => location.search.includes('missing=1'), undefined, {
    timeout: 4000,
  });
  const after = await h.page.locator(ROWS).allInnerTexts();
  if (after.length === 0) throw new Error('missing-only list is empty');

  await h.page.getByLabel('Missing only').uncheck();
  await h.page.waitForFunction(
    (n) => document.querySelectorAll('[data-testid="top-cards"] .toprow').length === n,
    before.length,
    { timeout: 4000 }
  );
  await h.screenshot('final_state');
}
