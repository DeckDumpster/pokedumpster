/**
 * Hand-written test for pokedumpster-0qu: multi-select copies on the card page
 * and bulk-set their condition. Charizard (base1/4) has two copies in the
 * fixture, so the checkbox column + select-all + bulk bar render. We select
 * all, bulk-set the condition, and assert every copy picked up the new value.
 * (Fail-first: before the feature there is no select-all checkbox, so the
 * first wait times out.)
 */
import type { ReplayHarness } from '../replay';

const SELECT_ALL = 'table thead input[type="checkbox"]';

export async function steps(h: ReplayHarness) {
  await h.wait_for_visible(SELECT_ALL, 6000);
  // Select every copy of this card.
  await h.page.locator(SELECT_ALL).check();
  // The bulk-edit bar appears once something is selected.
  await h.wait_for_visible('[data-testid="bulk-condition"]', 4000);

  // Bulk-set the condition across all selected copies.
  await h.select_by_label('[data-testid="bulk-condition"]', 'Moderately Played');

  // After the parallel apply + reload, every copy's Condition select reads the
  // new value — one action edited them all.
  await h.page.waitForFunction(
    () => {
      const sels = Array.from(
        document.querySelectorAll('td[data-label="Condition"] select'),
      ) as HTMLSelectElement[];
      return sels.length >= 2 && sels.every((s) => s.value === 'Moderately Played');
    },
    undefined,
    { timeout: 6000 },
  );
  await h.screenshot('final_state');
}
