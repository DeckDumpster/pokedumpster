<script lang="ts">
	import { page } from '$app/state';
	import favicon from '$lib/assets/favicon.svg';
	import { breadcrumbs, type Crumb } from '$lib/breadcrumbs.svelte';
	import Pokeball from '$lib/components/Pokeball.svelte';
	import { api } from '$lib/api';
	import type { BackupStatus } from '$lib/types/BackupStatus';

	let { children } = $props();

	// Layer 3 backup-staleness banner (pokedumpster-ivq.5). Passive visibility:
	// the host-side checker writes a freshness marker the server surfaces here.
	// Fire-and-forget — never block render, never error-page on a check failure.
	let backup = $state<BackupStatus | null>(null);
	$effect(() => {
		api.backupStatus()
			.then((s) => (backup = s))
			.catch(() => {});
	});
	const backupAgeLabel = $derived.by(() => {
		const secs = backup?.age_seconds;
		if (secs == null) return '';
		const hours = Number(secs) / 3600;
		return hours >= 48 ? `${Math.floor(hours / 24)} days` : `${Math.floor(hours)} hours`;
	});

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

{#if backup?.stale}
	<div class="backup-banner" role="alert">
		⚠️ Off-box backup is stale — last confirmed {backupAgeLabel} ago. Check the
		Litestream sidecar and <code>pkdump-backup-check</code>.
	</div>
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
	.backup-banner {
		padding: 0.6rem 1.25rem;
		background: #5a1e1e;
		color: #ffd9d9;
		border-bottom: 2px solid #e94560;
		font-size: 0.9rem;
		text-align: center;
	}
	.backup-banner code {
		background: rgba(0, 0, 0, 0.3);
		padding: 0.05rem 0.35rem;
		border-radius: 3px;
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
