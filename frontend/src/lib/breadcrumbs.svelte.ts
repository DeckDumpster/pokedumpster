export type Crumb = { label: string; href?: string };

// Per-page breadcrumb override. Set from a page's $effect when the leaf
// label can't be derived from the URL (e.g. /browse/me2pt5 → "Ascended
// Heroes"). Null falls back to the layout's URL-derived crumbs.
function createBreadcrumbs() {
	let crumbs = $state<Crumb[] | null>(null);
	return {
		get crumbs() {
			return crumbs;
		},
		set(c: Crumb[] | null) {
			crumbs = c;
		}
	};
}

export const breadcrumbs = createBreadcrumbs();
