import { defineConfig } from '@playwright/test';

/**
 * The deterministic browser tier — the safety net for the aesthetic
 * overhaul, and for anything else that needs a real DOM.
 *
 * This suite loads every route in `routes.json` at both breakpoints against
 * an isolated `--test` container instance and asserts layout properties.
 * There are no pixel baselines (sp-tk1oyu).
 *
 * `collection-window.spec.ts` runs in the same config without taking a
 * screenshot: some properties — "56,635 matches must not become 56,635 DOM
 * nodes" — need a browser and not a camera.
 *
 * Run it through `run.sh`, which stands the instance up and hands the port
 * over in PKDUMP_BASE_URL. Running `npx playwright test` directly works too,
 * against an instance you already have.
 *
 * Deliberately separate from `tests/ui` (the intents harness): that one needs
 * an ANTHROPIC_API_KEY and is non-deterministic by design. This one is pure
 * Playwright, offline, and belongs in CI.
 */
export default defineConfig({
	testDir: '.',
	testMatch: ['*.spec.ts'],
	// A shared server instance: serialise, so nothing races on the fixture.
	fullyParallel: false,
	workers: 1,
	timeout: 90_000,
	reporter: [['list'], ['html', { open: 'never', outputFolder: 'report' }]],
	outputDir: 'test-results',
	use: {
		baseURL: process.env.PKDUMP_BASE_URL ?? 'http://localhost:8080',
		ignoreHTTPSErrors: true,
		colorScheme: 'dark',
		timezoneId: 'UTC',
		locale: 'en-US',
		deviceScaleFactor: 1,
		screenshot: 'off',
		trace: 'retain-on-failure'
	},

	// The two widths the design record calls out: a desktop binder page and
	// the tablet/mobile breakpoint where /browse switches to its bottom sheet.
	projects: [
		{
			name: 'desktop-1440',
			use: { browserName: 'chromium', viewport: { width: 1440, height: 900 } }
		},
		{
			name: 'mobile-768',
			use: { browserName: 'chromium', viewport: { width: 768, height: 1024 } }
		}
	]
});
