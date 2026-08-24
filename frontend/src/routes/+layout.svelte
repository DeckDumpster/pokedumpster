<script lang="ts">
	import { page } from '$app/state';
	// The design-token layer — the single source of visual values for the whole
	// app. Imported here and nowhere else; see the header of tokens.css.
	import '$lib/styles/tokens.css';
	import { breadcrumbs, type Crumb } from '$lib/breadcrumbs.svelte';
	import Pokeball from '$lib/components/Pokeball.svelte';
	import { api } from '$lib/api';

	let { children } = $props();

	// NO BACKUP BANNER HERE. Backup staleness is OPERATOR information and this is a
	// tenant-facing, multi-tenant page (per Ryan, 2026-08-16: "having this on a banner
	// on a multi-tenant site makes absolutely no sense … there's no reason i would want
	// one of my friends — who can't do anything about it — to see such a warning").
	//
	// It is not merely noise to them: it is either meaningless or alarming, and neither
	// is actionable by someone who does not own the box. Worse, it was wrong for two
	// days straight while the backups were provably healthy, which is exactly how a
	// warning teaches its audience to ignore warnings.
	//
	// The operator channels remain and are the right ones: Pushover via
	// pkdump-alert@ (Layer 2) and the healthchecks.io dead-man (Layer 1). GET
	// /api/backup-status is untouched, so a control plane or dashboard can surface
	// this to someone who can act on it.

	// /collection and /sealed paint their own DD-style chrome (brand +
	// sticky search + burger) and run edge-to-edge; suppress the breadcrumb
	// header and the default main padding there.
	const pagesWithOwnChrome = ['/collection', '/sealed'];
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
	<link rel="icon" href="/favicon.svg" />
</svelte:head>

{#if !flush && !isHome}
	<header>
		<a class="logo" href="/" aria-label="Home">
			<Pokeball />
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
		background: var(--color-surface-page);
		color: var(--color-text);
	}
	header {
		display: flex;
		gap: 0.6rem;
		align-items: center;
		padding: 0.65rem 1.25rem;
		background: var(--color-surface-panel);
		border-bottom: 2px solid var(--color-border);
		font-size: 0.95rem;
	}
	.logo {
		display: inline-flex;
		align-items: center;
		text-decoration: none;
	}
	.crumbs {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-wrap: wrap;
		min-width: 0;
	}
	.crumbs a {
		color: var(--color-text-muted);
		text-decoration: none;
	}
	.crumbs a:hover {
		color: var(--color-text-accent);
	}
	.crumbs .current {
		color: var(--color-text);
		font-weight: 600;
	}
	.crumbs .sep {
		color: var(--color-text-decorative);
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
