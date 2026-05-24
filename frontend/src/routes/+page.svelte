<script lang="ts">
	import Pokeball from '$lib/components/Pokeball.svelte';

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
</style>
