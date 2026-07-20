<script lang="ts">
	import Pokeball from '$lib/components/Pokeball.svelte';
	import { api } from '$lib/api';

	const sections = [
		{ href: '/collection', label: 'Collection', desc: 'Browse every card you own.' },
		{ href: '/browse', label: 'Browse', desc: 'Open sets as virtual binder pages.' },
		{ href: '/binders', label: 'Binders', desc: 'Custom binders and their pages.' },
		{ href: '/decks', label: 'Decks', desc: 'Saved decklists.' },
		{ href: '/sealed', label: 'Sealed', desc: 'Sealed product (boxes, packs, ETBs).' },
		{ href: '/wishlist', label: 'Wishlist', desc: 'Cards you want.' },
		{ href: '/orders', label: 'Orders', desc: 'Purchases and their attached cards.' },
		{ href: '/recent', label: 'Recent', desc: 'Latest collection activity.' },
		{ href: '/ingest/csv', label: 'Import', desc: 'Bulk-import via CSV.' }
	];

	// Open dead-letter count — shows the "Unresolved" card only when there's a
	// backlog to work through. (pokedumpster-oq3i.5)
	let openCount = $state(0);
	$effect(() => {
		api.unresolvedList()
			.then((r) => (openCount = r.length))
			.catch(() => {});
	});
</script>

<svelte:head><title>PokeDumpster</title></svelte:head>

<header class="hero">
	<span class="logo"><Pokeball size={56} /></span>
	<div>
		<h1>PokeDumpster</h1>
		<p class="tagline">A Pokémon TCG collection tracker.</p>
	</div>
</header>

<nav class="capabilities" aria-label="Sections">
	{#each sections as s (s.href)}
		<a class="cap" href={s.href}>
			<span class="cap-label">{s.label}</span>
			<span class="cap-desc">{s.desc}</span>
		</a>
	{/each}
	{#if openCount > 0}
		<a class="cap alert" href="/ingest/unresolved">
			<span class="cap-label">Unresolved <span class="badge">{openCount}</span></span>
			<span class="cap-desc">Import rows waiting to be matched.</span>
		</a>
	{/if}
</nav>

<style>
	.hero {
		display: flex;
		align-items: center;
		gap: 1rem;
		margin-bottom: 2rem;
	}
	.hero h1 {
		margin: 0;
		font-size: 1.8rem;
		color: #e94560;
	}
	.tagline {
		margin: 0.25rem 0 0;
		color: #888;
		font-size: 0.95rem;
	}
	.logo {
		display: block;
		flex-shrink: 0;
	}
	.capabilities {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
		gap: 0.75rem;
	}
	.cap {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		padding: 0.9rem 1rem;
		background: #16213e;
		border: 1px solid #0f3460;
		border-radius: 8px;
		text-decoration: none;
		color: #e0e0e0;
		transition:
			border-color 0.12s,
			transform 0.08s;
	}
	.cap:hover {
		border-color: #e94560;
		transform: translateY(-1px);
	}
	.cap-label {
		font-weight: 600;
		color: #e94560;
	}
	.cap-desc {
		font-size: 0.85rem;
		color: #aab;
	}
	.cap.alert {
		border-color: #e9a045;
	}
	.cap.alert .cap-label {
		color: #e9a045;
	}
	.badge {
		display: inline-block;
		margin-left: 0.3rem;
		padding: 0.02rem 0.45rem;
		font-size: 0.75rem;
		border-radius: 999px;
		background: #e9a045;
		color: #16213e;
		font-weight: 700;
	}
</style>
