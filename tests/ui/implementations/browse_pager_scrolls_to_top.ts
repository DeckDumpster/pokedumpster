/**
 * Hand-written fail-first regression for pokedumpster-not: advancing a page in
 * binder browse should re-center the viewport at the top of the grid. The top
 * pager previously set the page without scrolling, leaving the user at the
 * bottom row of the new page.
 *
 * We click the TOP pager's Next via a dispatched click event so Playwright
 * does NOT auto-scroll it into view first (which would move the viewport for
 * us and mask the behaviour). Before the fix scrollY stays at the bottom;
 * after, gotoPage re-centers at the top once the new page renders.
 */
import type { ReplayHarness } from '../replay';

export async function steps(h: ReplayHarness) {
  await h.wait_for_visible('.slot', 6000);
  await h.wait_for_text('Page 1 of 3', 4000);

  // Scroll to the bottom row, near the pagers.
  await h.page.evaluate(() => window.scrollTo(0, document.body.scrollHeight));
  const scrolled = await h.page.evaluate(() => window.scrollY);
  if (scrolled < 100) throw new Error(`expected to be scrolled down first, scrollY=${scrolled}`);

  // Advance via the TOP pager without auto-scrolling it into view.
  await h.page
    .locator('.toppager button', { hasText: 'Next' })
    .dispatchEvent('click');
  await h.wait_for_text('Page 2 of 3', 4000);

  // The viewport must re-center near the top of the grid.
  await h.page.waitForFunction(() => window.scrollY < 50, undefined, { timeout: 4000 });
  await h.screenshot('final_state');
}
