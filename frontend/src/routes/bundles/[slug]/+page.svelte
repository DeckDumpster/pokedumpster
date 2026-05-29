<script lang="ts">
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { onMount } from 'svelte';

	// Bundles render through the unified container UX at /browse/[code]
	// now (pokedumpster-80q). Keep this route alive as a redirect so any
	// bookmarked /bundles/<slug> links still land on the right page.
	onMount(() => {
		const slug = page.params.slug;
		if (slug) goto(`/browse/${slug}`, { replaceState: true });
	});
</script>

<p>Redirecting…</p>
