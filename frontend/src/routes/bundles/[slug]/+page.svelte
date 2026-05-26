<script lang="ts">
	import { page } from '$app/state';
	import { api } from '$lib/api';
	import { variantLabel, variantColor, variantTag } from '$lib/variants.svelte';
	import type { BundleDetail } from '$lib/types/BundleDetail';
	import type { BundleSlot } from '$lib/types/BundleSlot';

	let detail = $state<BundleDetail | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let sessionBatchId = $state<number | null>(null);

	$effect(() => {
		const slug = page.params.slug;
		if (!slug) return;
		loading = true;
		api
			.bundle(slug)
			.then((d) => {
				detail = d;
				loading = false;
			})
			.catch((e) => {
				error = e instanceof Error ? e.message : String(e);
				loading = false;
			});
	});

	async function addOne(slot: BundleSlot) {
		if (!slot.printing_id) return;
		slot.owned_count += 1; // optimistic
		try {
			if (sessionBatchId === null) {
				sessionBatchId = await api.createBatch({
					batch_type: 'bundle_click',
					name: detail?.bundle.name ?? null
				});
			}
			await api.addCopy({
				printing_id: slot.printing_id,
				source: 'bundle_click',
				batch_id: sessionBatchId
			});
			if (detail) detail.bundle.owned_count = countOwned();
		} catch (e) {
			slot.owned_count -= 1;
			error = e instanceof Error ? e.message : String(e);
		}
	}

	function countOwned(): number {
		if (!detail) return 0;
		return detail.slots.filter((s) => s.owned_count > 0).length;
	}

	function pct(b: BundleDetail['bundle']): number {
		return b.slot_count > 0 ? Math.round((b.owned_count / b.slot_count) * 100) : 0;
	}
</script>

<svelte:head><title>{detail?.bundle.name ?? 'Bundle'} — PokeDumpster</title></svelte:head>

{#if loading}
	<p class="muted">Loading…</p>
{:else if error}
	<p class="error">{error}</p>
{:else if detail}
	<header>
		<h1>{detail.bundle.name}</h1>
		<div class="stat">
			<span>{detail.bundle.owned_count} / {detail.bundle.slot_count} collected</span>
			<div class="bar"><span style:width="{pct(detail.bundle)}%"></span></div>
		</div>
	</header>

	<p class="muted">
		Click <strong>+</strong> on a slot to add a copy. Each click logs to a single bundle-entry
		batch.
	</p>

	<div class="grid">
		{#each detail.slots as slot (slot.product_id)}
			<div class="tile" class:resolved={!!slot.printing_id} class:owned={slot.owned_count > 0}>
				{#if slot.image_url}
					<img class="img" src={slot.image_url} alt={slot.product_name} loading="lazy" />
				{:else}
					<div class="noart">{slot.product_name}</div>
				{/if}
				<div class="num">#{slot.collector_number.split('/')[0]}</div>
				<div class="name">
					{#if slot.set_code}
						<a class="cardlink" href="/card/{slot.set_code}/{slot.number ?? ''}"
							>{slot.card_name ?? slot.product_name}</a
						>
					{:else}
						{slot.product_name}
					{/if}
				</div>
				{#if slot.set_name}
					<div class="setline">
						<a class="setlink" href="/browse/{slot.set_code}">{slot.set_name}</a>
					</div>
				{/if}
				{#if slot.variant}
					<div class="vchipline">
						<span
							class="vchip"
							title={variantLabel(slot.variant)}
							style:--c={variantColor(slot.variant)}>{variantTag(slot.variant)}</span
						>
					</div>
				{/if}
				<div class="footer">
					{#if slot.printing_id}
						<button
							class="plus"
							onclick={() => addOne(slot)}
							title="Add one copy of {slot.card_name ?? slot.product_name}"
							aria-label="Add one"
						>
							+
						</button>
						<span class="count" class:gt0={slot.owned_count > 0}>×{slot.owned_count}</span>
					{:else}
						<span class="warn" title="Cross-group bridge did not resolve this product to a card."
							>unresolved</span
						>
					{/if}
				</div>
			</div>
		{/each}
	</div>
{/if}

<style>
	header {
		display: flex;
		gap: 1.5rem;
		align-items: baseline;
		flex-wrap: wrap;
		margin-bottom: 0.5rem;
	}
	h1 {
		color: #e94560;
		margin: 0;
	}
	.muted {
		color: #888;
	}
	.error {
		color: #e94560;
	}
	.stat {
		min-width: 180px;
	}
	.bar {
		height: 6px;
		background: #0f3460;
		border-radius: 3px;
		margin-top: 0.2rem;
		overflow: hidden;
	}
	.bar span {
		display: block;
		height: 100%;
		background: #4caf72;
	}
	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(160px, 1fr));
		gap: 0.6rem;
		margin-top: 1rem;
	}
	.tile {
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
		background: #16213e;
		border: 2px solid #0f3460;
		border-radius: 8px;
		padding: 0.5rem;
		font-size: 0.85rem;
	}
	.tile.owned {
		border-color: #4caf72;
	}
	.tile:not(.resolved) {
		opacity: 0.7;
	}
	.img {
		width: 100%;
		aspect-ratio: 5 / 7;
		object-fit: cover;
		object-position: top center;
		border-radius: 4px;
		background: #0d1424;
	}
	.noart {
		aspect-ratio: 5 / 7;
		display: flex;
		align-items: center;
		justify-content: center;
		text-align: center;
		padding: 0.3rem;
		background: #0d1424;
		border-radius: 4px;
	}
	.num {
		color: #888;
		font-size: 0.72rem;
	}
	.name {
		font-weight: 600;
		color: #e0e0e0;
		line-height: 1.2;
	}
	.cardlink {
		color: inherit;
		text-decoration: none;
	}
	.cardlink:hover {
		color: #e94560;
	}
	.setline {
		font-size: 0.72rem;
		color: #888;
	}
	.setlink {
		color: inherit;
		text-decoration: none;
	}
	.setlink:hover {
		color: #e94560;
	}
	.vchipline {
		margin-top: 0.1rem;
	}
	.vchip {
		display: inline-block;
		font-size: 0.6rem;
		font-weight: 700;
		letter-spacing: 0.04em;
		padding: 1px 5px;
		border-radius: 3px;
		border: 1px solid var(--c, #888);
		color: var(--c, #888);
		background: rgba(0, 0, 0, 0.2);
	}
	.footer {
		display: flex;
		align-items: center;
		justify-content: space-between;
		margin-top: auto;
		padding-top: 0.35rem;
	}
	.plus {
		background: #e94560;
		color: #fff;
		border: none;
		border-radius: 4px;
		padding: 0.15rem 0.55rem;
		font-weight: 700;
		cursor: pointer;
		font-size: 1rem;
		line-height: 1;
	}
	.plus:hover {
		background: #c63854;
	}
	.count {
		color: #666;
		font-variant-numeric: tabular-nums;
	}
	.count.gt0 {
		color: #4caf72;
		font-weight: 700;
	}
	.warn {
		color: #c0282d;
		font-size: 0.7rem;
		font-style: italic;
	}
</style>
