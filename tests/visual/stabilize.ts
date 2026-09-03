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
 * The instant every baseline is taken at. Arbitrary but fixed, so anything the
 * UI derives from "now" — a "n days ago", a relative age — is deterministic.
 *
 * It does NOT make the fixture's own dates deterministic. `shared.sqlite`'s are
 * fixed constants; `collection.sqlite`'s creation timestamps are stamped from
 * the clock when the fixture is built, so regenerating it moves the date cells
 * on /sealed, /recent, /batches and /batches/[id] and those baselines have to
 * be re-recorded. See tests/ui/fixtures/README.md, and pd-nzlj for the fix.
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
 * animation. `toHaveScreenshot` re-shoots until two frames match, so most of
 * this is about getting there quickly, not about correctness.
 *
 * The warm-up capture is the exception, and it *is* about correctness — see
 * the comment on it below.
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

	// Taking a fullPage shot is not a passive read of the page: to reach past
	// the viewport Chromium overrides the device metrics to the document's own
	// height, and that relayout can resolve text differently from the layout
	// that preceded it. On /search-help the inline <code> runs re-shape to a
	// monospace face one pixel shorter — computed font-family, font-size and
	// line-height are all unchanged, so only the inline box moves — which
	// reflows the lead paragraph and one <h2> and makes the document 4px
	// taller. It is a one-time step: every capture after the first agrees.
	//
	// That step is the whole flake. `toHaveScreenshot` shoots until two frames
	// match, so whether the suite saw 2557 or 2561 came down to whether the
	// relayout landed between its first two captures — fast box, first two
	// disagree and it gives up with "failed to take two consecutive stable
	// screenshots"; loaded box, both land cold and it compares 2557 against a
	// 2561 baseline. Same defect, two different failure texts, neither
	// reproducible on demand.
	//
	// So pay the step here, on a capture nobody looks at, and let the height
	// loop below confirm the page has stopped moving afterwards. Every route
	// then shoots from the warmed state its baseline was recorded in. Only
	// /search-help is measurably affected today (verified across all 24 routes
	// at both viewports), but nothing about the mechanism is specific to it,
	// which is why this lives in settle() and not in that route's entry.
	await page.screenshot({ fullPage: true });

	// A fullPage shot is taken at the document's height, so that height is part
	// of the screenshot's identity — and it can still be moving after everything
	// above has settled. Wait for the height to repeat before letting it shoot.
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
