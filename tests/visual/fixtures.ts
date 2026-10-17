/**
 * The `test` every spec imports (not `@playwright/test`'s own).
 *
 * Isolation mechanism for the parallel browser tier: each Playwright worker is
 * pinned to its own server instance and fixture database. PKDUMP_BASE_URLS is a
 * comma-separated list, one URL per worker (workers-lib.sh stands them up);
 * worker N uses entry N. With only PKDUMP_BASE_URL set there is one worker and
 * one instance, exactly as before.
 */
import { test as base, expect } from '@playwright/test';

const urls = (process.env.PKDUMP_BASE_URLS ?? '')
	.split(',')
	.map((u) => u.trim())
	.filter(Boolean);

export const test = base.extend({
	baseURL: async ({}, use, testInfo) => {
		const url = urls.length
			? urls[testInfo.parallelIndex]
			: (process.env.PKDUMP_BASE_URL ?? 'http://localhost:8080');
		if (!url) throw new Error(`no instance for worker ${testInfo.parallelIndex}: PKDUMP_BASE_URLS has ${urls.length}`);
		await use(url);
	}
});
export { expect };
