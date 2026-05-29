<script lang="ts">
	import { api } from '$lib/api';
	import type { ManualPrice } from '$lib/types/ManualPrice';

	// Per-printing manual price entry. The user can append timestamped
	// observations (default: now) — useful for printings TCGplayer doesn't
	// track (basep), or to backfill a time series from another source.
	// Effective-price rule in the catalog is gap-fill: TCGplayer wins when
	// present; manual fills the gap. See pkdump-db/src/manual_prices.rs.
	let {
		printingId,
		label,
		onClose,
		onChange
	}: {
		printingId: string;
		label: string;
		onClose: () => void;
		/** Called after any successful add/delete so the host page can
		 *  refresh its price chart. */
		onChange?: () => void;
	} = $props();

	let entries = $state<ManualPrice[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let busy = $state(false);

	let priceInput = $state('');
	let timeInput = $state(''); // datetime-local, optional
	let noteInput = $state('');

	async function load() {
		loading = true;
		try {
			entries = await api.manualPrices(printingId);
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		void printingId;
		load();
	});

	function formatTimestamp(iso: string): string {
		// Backend stores RFC3339 UTC (default) or whatever the user supplied.
		// Render in the user's local zone, no seconds.
		const d = new Date(iso);
		if (Number.isNaN(d.getTime())) return iso;
		return d.toLocaleString(undefined, {
			year: 'numeric',
			month: 'short',
			day: 'numeric',
			hour: '2-digit',
			minute: '2-digit'
		});
	}

	function fmtPrice(n: number): string {
		return `$${n.toFixed(2)}`;
	}

	async function submit(e: SubmitEvent) {
		e.preventDefault();
		const price = parseFloat(priceInput);
		if (!Number.isFinite(price) || price < 0) {
			error = 'Enter a non-negative price.';
			return;
		}
		busy = true;
		error = null;
		try {
			// datetime-local has no zone; treat as the user's local time and
			// convert to an ISO instant. Empty input → backend defaults to now.
			const observed_at = timeInput
				? new Date(timeInput).toISOString()
				: null;
			await api.addManualPrice({
				printing_id: printingId,
				price,
				observed_at,
				note: noteInput.trim() || null
			});
			priceInput = '';
			timeInput = '';
			noteInput = '';
			await load();
			onChange?.();
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		} finally {
			busy = false;
		}
	}

	async function remove(id: number) {
		busy = true;
		error = null;
		try {
			await api.deleteManualPrice(id);
			await load();
			onChange?.();
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		} finally {
			busy = false;
		}
	}
</script>

<svelte:window
	onkeydown={(e) => {
		if (e.key === 'Escape') onClose();
	}}
/>

<div class="backdrop" role="presentation" onclick={onClose}></div>
<div class="modal" role="dialog" aria-modal="true" aria-label="Manual prices for {label}">
	<header>
		<h3>Manual price — {label}</h3>
		<button class="x" onclick={onClose} aria-label="Close">×</button>
	</header>

	<form onsubmit={submit}>
		<div class="row">
			<label>
				<span>Price</span>
				<input
					type="number"
					step="0.01"
					min="0"
					required
					bind:value={priceInput}
					placeholder="e.g. 175.00"
					disabled={busy}
				/>
			</label>
			<label>
				<span>When <em>(optional)</em></span>
				<input
					type="datetime-local"
					bind:value={timeInput}
					disabled={busy}
				/>
			</label>
		</div>
		<label class="full">
			<span>Note <em>(optional)</em></span>
			<input
				type="text"
				bind:value={noteInput}
				placeholder="eBay sold, Goldin comp, …"
				disabled={busy}
			/>
		</label>
		<button class="save" type="submit" disabled={busy}>
			{busy ? 'Saving…' : 'Add price'}
		</button>
	</form>

	{#if error}<p class="error">{error}</p>{/if}

	<h4>History</h4>
	{#if loading}
		<p class="muted">Loading…</p>
	{:else if entries.length === 0}
		<p class="muted">No entries yet.</p>
	{:else}
		<ul>
			{#each entries as e (e.id)}
				<li>
					<span class="when">{formatTimestamp(e.observed_at)}</span>
					<span class="price">{fmtPrice(e.price)}</span>
					<span class="note">{e.note ?? ''}</span>
					<button
						class="del"
						disabled={busy}
						onclick={() => remove(e.id)}
						aria-label="Delete entry"
					>×</button>
				</li>
			{/each}
		</ul>
	{/if}
</div>

<style>
	.backdrop {
		position: fixed;
		inset: 0;
		background: rgba(0, 0, 0, 0.6);
		z-index: 100;
	}
	.modal {
		position: fixed;
		top: 50%;
		left: 50%;
		transform: translate(-50%, -50%);
		z-index: 101;
		box-sizing: border-box;
		width: 520px;
		max-width: 92vw;
		max-height: 85vh;
		overflow-y: auto;
		background: #16213e;
		border: 2px solid #0f3460;
		border-radius: 12px;
		padding: 1.25rem;
	}
	header {
		display: flex;
		justify-content: space-between;
		align-items: baseline;
	}
	h3 {
		margin: 0;
		color: #e94560;
		font-size: 1.05rem;
	}
	h4 {
		margin: 1.25rem 0 0.5rem;
		color: #888;
		font-size: 0.8rem;
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}
	.x {
		background: none;
		border: none;
		color: #888;
		font-size: 1.4rem;
		cursor: pointer;
		line-height: 1;
	}
	form {
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
		margin-top: 0.9rem;
	}
	.row {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: 0.6rem;
	}
	label {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		font-size: 0.8rem;
		color: #aaa;
	}
	label em {
		color: #666;
		font-style: normal;
	}
	label.full {
		grid-column: 1 / -1;
	}
	input {
		background: #1a1a2e;
		border: 1px solid #0f3460;
		color: #e0e0e0;
		padding: 0.45rem 0.55rem;
		border-radius: 6px;
		font: inherit;
	}
	input:focus {
		outline: none;
		border-color: #e94560;
	}
	.save {
		align-self: flex-start;
		background: #e94560;
		border: none;
		color: white;
		padding: 0.45rem 0.95rem;
		border-radius: 6px;
		cursor: pointer;
		font: inherit;
		margin-top: 0.2rem;
	}
	.save:disabled {
		opacity: 0.5;
		cursor: default;
	}
	.error {
		color: #e94560;
		font-size: 0.85rem;
		margin: 0.5rem 0 0;
	}
	.muted {
		color: #888;
		font-size: 0.85rem;
		margin: 0;
	}
	ul {
		list-style: none;
		padding: 0;
		margin: 0;
	}
	li {
		display: grid;
		grid-template-columns: auto auto 1fr auto;
		gap: 0.6rem;
		align-items: center;
		padding: 0.4rem 0;
		border-bottom: 1px solid #0f3460;
		font-size: 0.85rem;
	}
	.when {
		color: #aaa;
	}
	.price {
		color: #9fe7a0;
		font-variant-numeric: tabular-nums;
	}
	.note {
		color: #888;
		font-size: 0.8rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.del {
		background: none;
		border: none;
		color: #666;
		cursor: pointer;
		font-size: 1.1rem;
		line-height: 1;
		padding: 0 0.2rem;
	}
	.del:hover:not(:disabled) {
		color: #e94560;
	}

	@media (max-width: 540px) {
		.modal {
			top: auto;
			bottom: 0;
			left: 0;
			transform: none;
			width: 100%;
			max-width: 100%;
			max-height: 78vh;
			border-radius: 14px 14px 0 0;
		}
		.row {
			grid-template-columns: 1fr;
		}
	}
</style>
