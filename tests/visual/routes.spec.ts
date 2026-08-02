/**
 * One screenshot per route per viewport, compared against a committed
 * baseline. A diff fails; approving it is an explicit, reviewed act —
 * see README.md.
 */

import { readFileSync, readdirSync, statSync } from 'node:fs';
import { dirname, join, relative, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test, expect } from '@playwright/test';
import { stabilize, settle } from './stabilize';

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, '../..');

type Route = {
	id: string;
	path: string;
	about: string;
	waitFor?: string;
	mask?: string[];
};

const manifest: { routes: Route[]; unrepresented: Record<string, string> } = JSON.parse(
	readFileSync(join(here, 'routes.json'), 'utf8')
);

for (const route of manifest.routes) {
	test(`${route.id} — ${route.path}`, async ({ page }) => {
		await stabilize(page);

		const response = await page.goto(route.path, { waitUntil: 'domcontentloaded' });
		expect(
			response?.status() ?? 0,
			`${route.path} did not render — is the fixture instance up and seeded?`
		).toBeLessThan(400);

		await settle(page, route.waitFor);

		await expect(page).toHaveScreenshot(`${route.id}.png`, {
			fullPage: true,
			mask: (route.mask ?? []).map((selector) => page.locator(selector))
		});
	});
}

test('every route in the app has a baseline', () => {
	// The manifest is hand-written, so it can fall behind the app. This is what
	// stops that: add a +page.svelte and the visual suite fails until it is
	// either covered or explicitly written off in `unrepresented`.
	const routesDir = join(repoRoot, 'frontend/src/routes');

	const walk = (dir: string): string[] =>
		readdirSync(dir).flatMap((entry) => {
			const full = join(dir, entry);
			return statSync(full).isDirectory() ? walk(full) : [full];
		});

	const pages = walk(routesDir)
		.filter((f) => f.endsWith('+page.svelte'))
		.map((f) => 'routes/' + relative(routesDir, f).split(sep).join('/'));

	// A manifest path covers a page file when its segments line up, treating
	// `[param]` as a wildcard: /browse/sv3pt5 covers routes/browse/[set].
	const covers = (urlPath: string, pageFile: string): boolean => {
		const pageSegs = pageFile.replace(/^routes\//, '').replace(/\/?\+page\.svelte$/, '');
		const expected = pageSegs === '' ? [] : pageSegs.split('/');
		const actual = urlPath.split('/').filter(Boolean);
		if (expected.length !== actual.length) return false;
		return expected.every((seg, i) => (seg.startsWith('[') ? true : seg === actual[i]));
	};

	const uncovered = pages.filter(
		(pageFile) =>
			!(pageFile in manifest.unrepresented) &&
			!manifest.routes.some((r) => covers(r.path, pageFile))
	);
	expect(
		uncovered,
		'these routes ship with no visual baseline — add them to routes.json, ' +
			'or say in "unrepresented" why they need none'
	).toEqual([]);

	const ghosts = Object.keys(manifest.unrepresented).filter((f) => !pages.includes(f));
	expect(ghosts, 'routes.json writes off pages that no longer exist').toEqual([]);
});
