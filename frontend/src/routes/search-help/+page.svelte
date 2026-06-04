<script lang="ts">
	import { onMount } from 'svelte';
	import { api } from '$lib/api';
	import { breadcrumbs } from '$lib/breadcrumbs.svelte';
	import type { SearchVocabulary } from '$lib/types/SearchVocabulary';

	breadcrumbs.set([{ label: 'Collection', href: '/collection' }, { label: 'Search Help' }]);

	let vocab = $state<SearchVocabulary | null>(null);
	let error = $state<string | null>(null);

	onMount(async () => {
		try {
			vocab = await api.searchKeywords();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		}
	});

	// Operators are syntax, not data — documented inline.
	const operators: { op: string; meaning: string }[] = [
		{ op: ':', meaning: 'contains / matches (the default)' },
		{ op: '=', meaning: 'exact match' },
		{ op: '!=', meaning: 'not equal' },
		{ op: '<  >  <=  >=', meaning: 'numeric / ordinal comparison' }
	];

	const examples: { q: string; what: string }[] = [
		{ q: 't:fire hp>=200', what: 'Fire Pokémon with 200+ HP that you own' },
		{ q: 's:pfl rarity>=rare', what: 'Phantasmal Flames cards, rare or better' },
		{ q: 'is:missing s:sv3pt5 sub:ex', what: "151 'ex' cards you're still missing" },
		{ q: 'charizard is:holo', what: 'Holo Charizards in your collection' },
		{ q: 'price>20 added>=2026-01-01', what: 'Recent pickups worth over $20' },
		{ q: 'pikachu qty>=2', what: 'Printings you own 2 or more copies of' },
		{ q: 't:water or t:lightning -is:graded', what: 'Water/Lightning, excluding graded copies' },
		{ q: 'order:price direction:desc', what: 'Everything, most valuable first' }
	];

	function aliasList(aliases: string[]): string {
		return aliases.join(', ');
	}
</script>

<svelte:head><title>Search Help — PokeDumpster</title></svelte:head>

<div class="wrap">
	<h1>Search syntax</h1>
	<p class="lead">
		The collection search bar speaks a Scryfall-style query language: combine keywords with
		<code>AND</code> (just a space), <code>or</code>, <code>-</code> (not), and parentheses. By
		default it searches the cards you own; add <code>is:missing</code> to include cards you don't.
	</p>

	{#if error}
		<p class="error">Couldn't load the keyword list: {error}</p>
	{/if}

	<section>
		<h2>Operators</h2>
		<table>
			<tbody>
				{#each operators as o (o.op)}
					<tr><td class="mono">{o.op}</td><td>{o.meaning}</td></tr>
				{/each}
			</tbody>
		</table>
	</section>

	<section>
		<h2>Examples</h2>
		<table>
			<tbody>
				{#each examples as ex (ex.q)}
					<tr><td class="mono">{ex.q}</td><td>{ex.what}</td></tr>
				{/each}
			</tbody>
		</table>
	</section>

	{#if vocab}
		<section>
			<h2>Keywords</h2>
			<table>
				<thead><tr><th>Keyword</th><th>Operators</th><th>Description</th></tr></thead>
				<tbody>
					{#each vocab.keywords as k (k.canonical)}
						<tr>
							<td class="mono">{aliasList(k.aliases)}</td>
							<td class="mono ops">{k.operators.join(' ')}</td>
							<td>{k.help ?? ''}</td>
						</tr>
					{/each}
				</tbody>
			</table>
		</section>

		<section>
			<h2><code>is:</code> flags</h2>
			<table>
				<tbody>
					{#each vocab.flags as f (f.flag)}
						<tr><td class="mono">is:{f.flag}</td><td>{f.help ?? ''}</td></tr>
					{/each}
				</tbody>
			</table>
		</section>
	{:else if !error}
		<p class="muted">Loading keywords…</p>
	{/if}
</div>

<style>
	.wrap {
		max-width: 60rem;
		margin: 0 auto;
		padding: 1.5rem 1.25rem 4rem;
	}
	h1 {
		margin: 0 0 0.5rem;
	}
	h2 {
		margin: 2rem 0 0.6rem;
		font-size: 1.1rem;
		color: #cdd3ff;
	}
	.lead {
		color: #b8bcd0;
		line-height: 1.5;
		max-width: 48rem;
	}
	table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.92rem;
	}
	th {
		text-align: left;
		color: #9aa0bd;
		font-weight: 600;
		border-bottom: 1px solid #2a2f4a;
		padding: 0.4rem 0.6rem;
	}
	td {
		padding: 0.35rem 0.6rem;
		border-bottom: 1px solid #20243a;
		vertical-align: top;
	}
	.mono {
		font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
		color: #ffd66b;
		white-space: nowrap;
	}
	.ops {
		color: #8fb7ff;
	}
	code {
		font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
		background: #20243a;
		padding: 0.05rem 0.3rem;
		border-radius: 0.25rem;
		color: #ffd66b;
	}
	.error {
		color: #ff8a8a;
	}
	.muted {
		color: #9aa0bd;
	}
</style>
