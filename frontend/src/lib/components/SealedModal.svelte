<script lang="ts">
	import { fade } from 'svelte/transition';
	import { api } from '$lib/api';
	import { money } from '$lib/format';
	import type { SealedEntry } from '$lib/types/SealedEntry';

	// Sealed-product detail/edit modal — the sealed analogue of CardModal +
	// CardDetailView. The parent hands us the row it already has (the list
	// query carries every field, market price included), and every edit PUTs a
	// single field and swaps in the fresh SealedEntry the server returns, so
	// the modal never drifts from the backend. `onMutate` lets the parent
	// re-run its list only when something actually changed.
	let {
		entry: initial,
		onClose,
		onMutate
	}: {
		entry: SealedEntry;
		onClose: () => void;
		/** Fired after any successful edit or delete so the host list refreshes. */
		onMutate?: () => void;
	} = $props();

	// The row we render: each save swaps in the fresh SealedEntry the server
	// returns (`edited`), so market/value update live without waiting for the
	// host list to reload; until the first edit we show the prop the host
	// handed us. Deriving (rather than seeding $state from the prop) keeps the
	// prop reactive and avoids the state_referenced_locally warning.
	let edited = $state<SealedEntry | null>(null);
	const entry = $derived(edited ?? initial);

	// The status set enforced by the sealed_collection CHECK constraint
	// (schema_user.sql) — the DB rejects anything else, so the picker mirrors it.
	const STATUSES = ['owned', 'listed', 'sold', 'traded', 'gifted', 'opened'];

	let error = $state<string | null>(null);
	// Per-control in-flight key so only the field being saved locks, and a ✓
	// can flash beside exactly what changed (mirrors CardDetailView).
	let savingKeys = $state<Set<string>>(new Set());
	const isSaving = (key: string) => savingKeys.has(key);
	let savedKey = $state<string | null>(null);
	let savedTimer: ReturnType<typeof setTimeout> | undefined;
	function flashSaved(key: string) {
		savedKey = key;
		clearTimeout(savedTimer);
		savedTimer = setTimeout(() => (savedKey = null), 1600);
	}

	async function save(patch: Partial<import('$lib/types/SealedEdit').SealedEdit>, key: string) {
		savingKeys = new Set(savingKeys).add(key);
		error = null;
		try {
			edited = await api.updateSealed(entry.id, patch);
			onMutate?.();
			flashSaved(key);
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			const next = new Set(savingKeys);
			next.delete(key);
			savingKeys = next;
		}
	}

	// Number inputs come back as strings; empty means "clear" for optional
	// prices, but quantity is NOT NULL so an empty box is ignored.
	function saveQuantity(raw: string) {
		const n = Math.max(1, Math.floor(Number(raw) || 1));
		void save({ quantity: n }, 'quantity');
	}
	function savePrice(field: 'purchase_price' | 'sale_price', raw: string) {
		void save({ [field]: raw === '' ? undefined : Number(raw) }, field);
	}

	async function remove() {
		if (!confirm('Remove this sealed product from your collection?')) return;
		savingKeys = new Set(savingKeys).add('delete');
		error = null;
		try {
			await api.deleteSealed(entry.id);
			onMutate?.();
			onClose();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
			const next = new Set(savingKeys);
			next.delete('delete');
			savingKeys = next;
		}
	}

	const value = $derived(entry.market_price != null ? entry.market_price * entry.quantity : null);
</script>

<svelte:window
	onkeydown={(e) => {
		if (e.key === 'Escape') onClose();
	}}
/>

{#snippet savedTick(key: string)}
	{#if savedKey === key}<span class="cellSaved" transition:fade={{ duration: 120 }}>✓</span>{/if}
{/snippet}

<div class="backdrop" role="presentation" onclick={onClose}></div>
<div class="modal" role="dialog" aria-modal="true" aria-label="Sealed product detail">
	<div class="controls">
		<a
			class="tcgp"
			href="https://www.tcgplayer.com/product/{entry.product_id}"
			target="_blank"
			rel="noopener"
			title="Open on TCGplayer">TCG↗</a
		>
		<button class="x" onclick={onClose} aria-label="Close">×</button>
	</div>
	<div class="body">
		<div class="detail">
			<div class="art">
				{#if entry.image_url}
					<img src={entry.image_url} alt={entry.name} />
				{:else}
					<div class="noart">No image</div>
				{/if}
			</div>
			<div class="info">
				<h1>{entry.name}</h1>
				<p class="sub">
					<span class="cat">{entry.category.replace(/_/g, ' ')}</span>
					{#if entry.set_code}· <span>{entry.set_code.toUpperCase()}</span>{/if}
				</p>
				<dl>
					<dt>Market</dt>
					<dd>{money(entry.market_price)}</dd>
					<dt>Value</dt>
					<dd title="Market × quantity">{money(value)}</dd>
					<dt>Added</dt>
					<dd>{entry.added_at.slice(0, 10)}</dd>
				</dl>
			</div>
		</div>

		{#if error}<p class="error">{error}</p>{/if}

		<section>
			<h2>Details</h2>
			<div class="fields">
				<label class="field">
					<span class="flabel">Quantity {@render savedTick('quantity')}</span>
					<input
						type="number"
						min="1"
						step="1"
						value={entry.quantity}
						disabled={isSaving('quantity')}
						onchange={(e) => saveQuantity(e.currentTarget.value)}
					/>
				</label>
				<label class="field">
					<span class="flabel">Status {@render savedTick('status')}</span>
					<select
						value={entry.status}
						disabled={isSaving('status')}
						onchange={(e) => save({ status: e.currentTarget.value }, 'status')}
					>
						{#each STATUSES as s (s)}<option value={s}>{s}</option>{/each}
					</select>
				</label>
				<label class="field">
					<span class="flabel">Condition {@render savedTick('condition')}</span>
					<input
						type="text"
						placeholder="e.g. Sealed, box dented"
						value={entry.condition ?? ''}
						disabled={isSaving('condition')}
						onchange={(e) => save({ condition: e.currentTarget.value }, 'condition')}
					/>
				</label>
				<label class="field">
					<span class="flabel">Paid {@render savedTick('purchase_price')}</span>
					<input
						type="number"
						min="0"
						step="0.01"
						value={entry.purchase_price ?? ''}
						disabled={isSaving('purchase_price')}
						onchange={(e) => savePrice('purchase_price', e.currentTarget.value)}
					/>
				</label>
				<label class="field">
					<span class="flabel">Sale price {@render savedTick('sale_price')}</span>
					<input
						type="number"
						min="0"
						step="0.01"
						value={entry.sale_price ?? ''}
						disabled={isSaving('sale_price')}
						onchange={(e) => savePrice('sale_price', e.currentTarget.value)}
					/>
				</label>
			</div>
			<label class="field wide">
				<span class="flabel">Notes {@render savedTick('notes')}</span>
				<input
					type="text"
					placeholder="Notes — e.g. bought at prerelease, factory sealed"
					value={entry.notes ?? ''}
					disabled={isSaving('notes')}
					onchange={(e) => save({ notes: e.currentTarget.value }, 'notes')}
				/>
			</label>
		</section>

		<section class="danger">
			<button class="delete" disabled={isSaving('delete')} onclick={remove}>Delete from collection</button>
		</section>
	</div>
</div>

<style>
	.backdrop {
		position: fixed;
		inset: 0;
		background: var(--color-scrim);
		z-index: 100;
	}
	.modal {
		position: fixed;
		top: 4vh;
		left: 50%;
		transform: translateX(-50%);
		z-index: 101;
		width: 620px;
		box-sizing: border-box;
		max-width: 92vw;
		max-height: 92vh;
		display: flex;
		flex-direction: column;
		overflow: hidden;
		background: var(--color-surface-page);
		border: 2px solid var(--color-border);
		border-radius: 12px;
	}
	.controls {
		position: absolute;
		top: 8px;
		right: 8px;
		z-index: 3;
		display: flex;
		gap: 6px;
		align-items: center;
	}
	.x,
	.tcgp {
		height: 32px;
		display: flex;
		align-items: center;
		justify-content: center;
		background: var(--color-scrim);
		border: 1px solid var(--color-border);
		border-radius: 16px;
		color: var(--color-text);
		line-height: 1;
		cursor: pointer;
		padding: 0 10px;
		text-decoration: none;
	}
	.x {
		width: 32px;
		padding: 0;
		font-size: 1.4rem;
	}
	.tcgp {
		font-size: 0.8rem;
		color: var(--color-info-text);
	}
	.x:hover,
	.tcgp:hover {
		background: var(--color-accent);
		color: var(--color-on-accent);
	}
	.body {
		flex: 1;
		overflow-y: auto;
		overflow-x: hidden;
		padding: 1.25rem 1.5rem 1.5rem;
	}
	.detail {
		display: flex;
		gap: 1.5rem;
		flex-wrap: wrap;
	}
	.art img {
		width: 200px;
		max-width: 60vw;
		border-radius: 10px;
		background: var(--color-surface-well);
	}
	.noart {
		width: 200px;
		height: 200px;
		display: flex;
		align-items: center;
		justify-content: center;
		background: var(--color-surface-panel);
		border-radius: 10px;
		color: var(--color-text-subtle);
	}
	.info {
		flex: 1;
		min-width: 220px;
	}
	h1 {
		color: var(--color-text-accent);
		margin: 0;
		font-size: 1.4rem;
	}
	.sub {
		color: var(--color-text-subtle);
		margin: 0.25rem 0 1rem;
	}
	.cat {
		text-transform: capitalize;
	}
	dl {
		display: grid;
		grid-template-columns: auto 1fr;
		gap: 0.3rem 1rem;
		margin: 0;
	}
	dt {
		color: var(--color-text-subtle);
		font-size: 0.85rem;
	}
	dd {
		margin: 0;
		font-variant-numeric: tabular-nums;
	}
	.error {
		color: var(--color-text-accent);
	}
	section {
		margin-top: 1.75rem;
	}
	h2 {
		color: var(--color-text-accent);
		font-size: 1.1rem;
		margin: 0 0 0.6rem;
	}
	.fields {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
		gap: 0.75rem;
	}
	.field {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		position: relative;
	}
	.field.wide {
		margin-top: 0.75rem;
	}
	.flabel {
		color: var(--color-text-subtle);
		font-size: 0.75rem;
		text-transform: uppercase;
	}
	.field input,
	.field select {
		background: var(--color-surface-panel);
		border: 1px solid var(--color-border);
		color: var(--color-text);
		border-radius: 6px;
		padding: 0.4rem 0.5rem;
		font: inherit;
	}
	.cellSaved {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 14px;
		height: 14px;
		border-radius: 50%;
		background: var(--color-success);
		color: var(--color-on-success);
		font-size: 0.6rem;
		line-height: 1;
		vertical-align: middle;
	}
	.danger {
		border-top: 1px solid var(--color-border);
		padding-top: 1rem;
	}
	.delete {
		background: none;
		border: 1px solid var(--color-danger-border);
		color: var(--color-danger-text);
		padding: 0.4rem 0.8rem;
		border-radius: 6px;
		cursor: pointer;
		font: inherit;
	}
	.delete:hover:not(:disabled) {
		background: var(--color-danger-surface);
		color: var(--color-text-strong);
	}
	.delete:disabled {
		opacity: 0.5;
		cursor: default;
	}

	@media (max-width: 540px) {
		.modal {
			top: auto;
			bottom: 0;
			left: 0;
			transform: none;
			width: 100%;
			max-width: 100%;
			border-radius: 14px 14px 0 0;
		}
		.body {
			padding: 1rem 0.85rem 1.25rem;
		}
	}
</style>
