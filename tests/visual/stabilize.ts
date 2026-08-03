/**
 * Everything that has to be nailed down before a screenshot is a baseline
 * rather than a photograph of one particular afternoon.
 *
 * A visual-regression suite is only worth having if a failure means "the UI
 * changed". Each control here removes a source of pixel churn that has nothing
 * to do with the design system:
 *
 *   clock      — the layout header renders a backup age in hours/days
 *   backup     — ...and only renders that banner at all when the host's
 *                Litestream marker is stale, which is host state, not UI state
 *   card art   — the fixture points at images.pokemontcg.io; the test box may
 *                be offline, and a CDN is not the thing under test
 *   motion     — CSS transitions/animations, and the caret
 *   scrollbars — width differs with the overlay-scrollbar setting
 *
 * Nothing here changes application behaviour; it constrains the environment
 * the application runs in.
 */

import type { Page } from '@playwright/test';

/**
 * The instant every baseline is taken at. Arbitrary but fixed — the fixture's
 * own timestamps are fixed constants too (tests/ui/fixtures/README.md), so a
 * frozen clock makes every derived "n days ago" deterministic.
 */
export const FROZEN_TIME = new Date('2026-01-15T12:00:00Z');

/**
 * Stand-in for card art: a flat card-shaped rectangle at the standard 245x342
 * TCG aspect. Deterministic, offline, and visually quiet so the chrome — which
 * is what this suite is actually guarding — is what you see in a diff.
 */
const CARD_PLACEHOLDER =
	`<svg xmlns="http://www.w3.org/2000/svg" width="245" height="342" viewBox="0 0 245 342">` +
	`<rect width="245" height="342" rx="12" fill="#3a3a52"/>` +
	`<rect x="14" y="14" width="217" height="314" rx="6" fill="#2c2c40"/>` +
	`</svg>`;

/** Backup is fresh: no banner, no age string, no dependence on the host. */
const FRESH_BACKUP = {
	last_ok_epoch: Math.floor(FROZEN_TIME.getTime() / 1000) - 3600,
	age_seconds: 3600,
	stale: false,
	stale_threshold_seconds: 172800
};

/**
 * Apply every stabilisation control to a page. Call before the first
 * `page.goto` — routes and the clock must be installed before navigation.
 */
export async function stabilize(page: Page): Promise<void> {
	// setFixedTime, not install: pinning Date.now() is the whole requirement,
	// and a fully faked clock would also freeze requestAnimationFrame, leaving
	// every Chart.js canvas baselined mid-animation.
	await page.clock.setFixedTime(FROZEN_TIME);

	// Host state, not UI state. Stubbed rather than masked so the banner's
	// absence is itself part of the baseline: if a future change makes it
	// render unconditionally, that is a real regression and shows up.
	await page.route('**/api/backup-status', (route) =>
		route.fulfill({ json: FRESH_BACKUP })
	);

	await page.route('**/*', async (route) => {
		const url = new URL(route.request().url());
		const local = url.hostname === 'localhost' || url.hostname === '127.0.0.1';
		if (local) return route.fallback();

		// Anything off-box is either card art (placeholder it) or a font/
		// analytics/ping we do not want a baseline to depend on (abort it).
		if (route.request().resourceType() === 'image') {
			return route.fulfill({
				status: 200,
				contentType: 'image/svg+xml',
				body: CARD_PLACEHOLDER
			});
		}
		return route.abort();
	});

	// addInitScript, not addStyleTag: a style tag added before the first goto
	// belongs to about:blank and is thrown away by the navigation. This runs on
	// every document the page loads.
	await page.addInitScript(() => {
		const inject = () => {
			const style = document.createElement('style');
			style.textContent = `
				*, *::before, *::after {
					transition-duration: 0s !important;
					animation-duration: 0s !important;
					animation-delay: 0s !important;
					scroll-behavior: auto !important;
				}
				/* Overlay vs classic scrollbars shift every fixed-width layout
				   by however many pixels the host happens to reserve. */
				::-webkit-scrollbar { width: 0 !important; height: 0 !important; }
				html { scrollbar-width: none !important; }
			`;
			document.head.appendChild(style);
		};
		if (document.head) inject();
		else document.addEventListener('DOMContentLoaded', inject, { once: true });
	});
}

/**
 * Wait until the page has stopped moving: data fetched, webfonts resolved,
 * and — for the routes that draw one — Chart.js finished its opening
 * animation. `toHaveScreenshot` re-shoots until two frames match, so this is
 * about getting there quickly, not about correctness.
 */
export async function settle(page: Page, waitFor?: string): Promise<void> {
	if (waitFor) await page.locator(waitFor).first().waitFor({ state: 'visible' });
	await page.waitForLoadState('networkidle');
	await page.evaluate(() => document.fonts.ready.then(() => undefined));
	// Chart.js animates on a rAF loop the clock freeze does not drive; two
	// frames is enough for it to have committed its final geometry.
	await page.evaluate(
		() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)))
	);

	// A fullPage shot is taken at the document's height, so that height is part
	// of the screenshot's identity — and it can still be moving after everything
	// above has settled. /search-help at 1440 grows by four trailing pixels as
	// its last table commits, which is enough for `toHaveScreenshot` to give up
	// with "failed to take two consecutive stable screenshots" even though the
	// two frames are pixel-identical everywhere they overlap. Wait for the
	// height to repeat before letting it shoot.
	await page.waitForFunction(
		() => {
			const w = window as unknown as { __pdH?: number; __pdN?: number };
			const h = document.documentElement.scrollHeight;
			if (w.__pdH === h) w.__pdN = (w.__pdN ?? 0) + 1;
			else {
				w.__pdH = h;
				w.__pdN = 0;
			}
			return (w.__pdN ?? 0) >= 3;
		},
		null,
		{ polling: 100, timeout: 5000 }
	);
}
