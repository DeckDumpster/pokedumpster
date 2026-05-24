<script lang="ts">
	import { page } from '$app/state';
	import favicon from '$lib/assets/favicon.svg';
	import { breadcrumbs, type Crumb } from '$lib/breadcrumbs.svelte';

	let { children } = $props();

	// /collection paints its own DD-style chrome (brand + sticky search +
	// burger) and runs edge-to-edge; suppress the breadcrumb header and
	// the default main padding there.
	const pagesWithOwnChrome = ['/collection'];
	const flush = $derived(pagesWithOwnChrome.includes(page.url.pathname));
	// The home page renders its own capabilities list and doesn't need
	// the shared breadcrumb chrome above it.
	const isHome = $derived(page.url.pathname === '/');

	// Human-readable label for a URL segment. Pages can override the full
	// trail via setBreadcrumbs() when a leaf label can't be derived (e.g.
	// /browse/me2pt5 → "Ascended Heroes").
	function labelFor(seg: string): string {
		return seg
			.split('-')
			.map((w) => (w.length === 0 ? w : w.charAt(0).toUpperCase() + w.slice(1)))
			.join(' ');
	}

	const derivedCrumbs = $derived.by((): Crumb[] => {
		const segs = page.url.pathname.split('/').filter(Boolean);
		return segs.map((seg, i) => ({
			label: labelFor(seg),
			href: i < segs.length - 1 ? '/' + segs.slice(0, i + 1).join('/') : undefined
		}));
	});

	const crumbs = $derived<Crumb[]>(breadcrumbs.crumbs ?? derivedCrumbs);
</script>

<svelte:head>
	<link rel="icon" href={favicon} />
</svelte:head>

{#if !flush && !isHome}
	<header>
		<a class="logo" href="/" aria-label="Home">
			<svg viewBox="0 0 24 24" width="22" height="22" xmlns="http://www.w3.org/2000/svg">
				<circle cx="12" cy="12" r="10.5" fill="#fff" stroke="#0f3460" stroke-width="1.2" />
				<path d="M1.5 12 A 10.5 10.5 0 0 1 22.5 12 Z" fill="#e94560" />
				<rect x="1.5" y="11.25" width="21" height="1.5" fill="#0f3460" />
				<circle cx="12" cy="12" r="3.2" fill="#fff" stroke="#0f3460" stroke-width="1.4" />
				<circle cx="12" cy="12" r="1.3" fill="#fff" stroke="#0f3460" stroke-width="0.7" />
			</svg>
		</a>
		<nav class="crumbs" aria-label="Breadcrumb">
			{#each crumbs as crumb, i (i + crumb.label)}
				<span class="sep" aria-hidden="true">›</span>
				{#if crumb.href}
					<a href={crumb.href}>{crumb.label}</a>
				{:else}
					<span class="current" aria-current="page">{crumb.label}</span>
				{/if}
			{/each}
		</nav>
	</header>
{/if}

<main class:flush>
	{@render children()}
</main>

<style>
	:global(body) {
		margin: 0;
		font-family: system-ui, -apple-system, 'Segoe UI', Roboto, sans-serif;
		background: #1a1a2e;
		color: #e0e0e0;
	}
	header {
		display: flex;
		gap: 0.6rem;
		align-items: center;
		padding: 0.65rem 1.25rem;
		background: #16213e;
		border-bottom: 2px solid #0f3460;
		font-size: 0.95rem;
	}
	.logo {
		display: inline-flex;
		align-items: center;
		text-decoration: none;
	}
	.logo svg {
		display: block;
	}
	.crumbs {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-wrap: wrap;
		min-width: 0;
	}
	.crumbs a {
		color: #b8c1d9;
		text-decoration: none;
	}
	.crumbs a:hover {
		color: #e94560;
	}
	.crumbs .current {
		color: #e0e0e0;
		font-weight: 600;
	}
	.crumbs .sep {
		color: #4a5680;
	}
	main {
		padding: 1.5rem;
	}
	/* Pages with their own sticky chrome (currently /collection) get an
	   edge-to-edge main so the chrome can pin to the viewport edges. */
	main.flush {
		padding: 0;
	}
	@media (max-width: 540px) {
		header {
			padding: 0.55rem 0.8rem;
			font-size: 0.88rem;
		}
		main {
			padding: 0.6rem;
		}
	}
</style>
