import { page } from '$app/state';

export type Crumb = { label: string; href?: string };

// Per-page breadcrumb override. Set from a page's script (synchronously,
// not in an async $effect) when the leaf label can't be derived from the
// URL — e.g. /browse/me2pt5 → "Ascended Heroes" or /card/sv10/153 →
// "Browse › Destined Rivals › Team Rocket's Porygon #153".
//
// The store tags every override with the pathname that set it. Reading
// `crumbs` from a different URL returns null — the override "expires" the
// moment SvelteKit's router moves to a new page, so stale crumbs from
// the previously-mounted page can't leak into the next render. This is
// what eliminates the "Card › Base1 › 4" → "Browse › Base › …" flash
// the URL-derived fallback used to produce while async data loaded.
function createBreadcrumbs() {
	let entry = $state<{ path: string; crumbs: Crumb[] } | null>(null);
	return {
		// Layout reads this. Returns null whenever the URL doesn't match
		// the override's path — the layout then falls back to its
		// URL-derived crumbs.
		get crumbs(): Crumb[] | null {
			if (entry && entry.path === page.url.pathname) return entry.crumbs;
			return null;
		},
		// Pages call this to register their own crumbs for the current URL.
		// Setting null clears the override explicitly (rarely needed — the
		// path check above handles cross-page invalidation for free).
		set(c: Crumb[] | null) {
			if (c === null) {
				entry = null;
				return;
			}
			entry = { path: page.url.pathname, crumbs: c };
		}
	};
}

export const breadcrumbs = createBreadcrumbs();
