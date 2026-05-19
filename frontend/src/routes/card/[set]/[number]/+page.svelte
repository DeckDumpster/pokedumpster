<script lang="ts">
	import { page } from '$app/state';
	import CardDetailView from '$lib/components/CardDetailView.svelte';
</script>

<svelte:head><title>Card — PokeDumpster</title></svelte:head>

{#if page.params.set && page.params.number}
	<CardDetailView setCode={page.params.set} number={page.params.number} />
{/if}
