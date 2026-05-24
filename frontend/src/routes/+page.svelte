<script lang="ts">
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
	<svg
		class="logo"
		viewBox="0 0 24 24"
		width="56"
		height="56"
		xmlns="http://www.w3.org/2000/svg"
		aria-hidden="true"
	>
		<circle cx="12" cy="12" r="10.5" fill="#fff" stroke="#0f3460" stroke-width="1.2" />
		<path d="M1.5 12 A 10.5 10.5 0 0 1 22.5 12 Z" fill="#e94560" />
		<rect x="1.5" y="11.25" width="21" height="1.5" fill="#0f3460" />
		<circle cx="12" cy="12" r="3.2" fill="#fff" stroke="#0f3460" stroke-width="1.4" />
		<circle cx="12" cy="12" r="1.3" fill="#fff" stroke="#0f3460" stroke-width="0.7" />
	</svg>
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
