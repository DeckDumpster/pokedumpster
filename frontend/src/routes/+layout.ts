// SPA mode: render entirely in the browser. The Axum server serves the
// static build and provides the API; there is no Node server-side render.
export const ssr = false;
export const prerender = false;

import { variants } from '$lib/variants.svelte';
import { conditions } from '$lib/conditions.svelte';

// Block initial render until the variants display-metadata table is
// loaded — every page that renders a variant chip, modal row, or
// collection table tag depends on the map being populated. One small
// fetch up-front is far less painful than every label briefly
// rendering as the raw code.
export const load = async (): Promise<void> => {
	await Promise.all([variants.load(), conditions.load()]);
};
