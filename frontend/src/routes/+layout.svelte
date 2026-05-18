<script lang="ts">
	import favicon from '$lib/assets/favicon.svg';

	let { children } = $props();

	const nav = [
		{ href: '/', label: 'Home' },
		{ href: '/collection', label: 'Collection' },
		{ href: '/browse', label: 'Browse' },
		{ href: '/binders', label: 'Binders' },
		{ href: '/decks', label: 'Decks' },
		{ href: '/sealed', label: 'Sealed' },
		{ href: '/sets', label: 'Sets' }
	];
</script>

<svelte:head>
	<link rel="icon" href={favicon} />
</svelte:head>

<header>
	<a class="brand" href="/">PokeDumpster</a>
	<nav>
		{#each nav as item (item.href)}
			<a href={item.href}>{item.label}</a>
		{/each}
	</nav>
</header>

<main>
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
		gap: 1.5rem;
		align-items: baseline;
		padding: 1rem 1.5rem;
		background: #16213e;
		border-bottom: 2px solid #0f3460;
	}
	.brand {
		font-weight: 700;
		font-size: 1.2rem;
		color: #e94560;
		text-decoration: none;
	}
	nav {
		display: flex;
		gap: 1rem;
		flex-wrap: wrap;
	}
	nav a {
		color: #e0e0e0;
		text-decoration: none;
		font-size: 0.9rem;
	}
	nav a:hover {
		color: #e94560;
	}
	main {
		padding: 1.5rem;
	}
</style>
