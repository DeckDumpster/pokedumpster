<script lang="ts">
	import { goto } from '$app/navigation';
	import { api, variantLabel } from '$lib/api';
	import type { PrintingInfo } from '$lib/types/PrintingInfo';

	type Line = {
		setCode: string;
		number: string;
		nameHint: string;
		cardName: string;
		printings: PrintingInfo[];
		printingId: string;
		quantity: number;
		price: string;
		error: string;
	};

	// Order metadata.
	let source = $state('tcgplayer');
	let seller = $state('');
	let orderDate = $state('');
	let total = $state('');
	let notes = $state('');

	let lines = $state<Line[]>([]);
	let pasteText = $state('');
	let committing = $state(false);
	let error = $state<string | null>(null);

	const SOURCES = ['tcgplayer', 'ebay', 'pokemoncenter', 'lgs', 'other'];

	function blankLine(): Line {
		return {
			setCode: '',
			number: '',
			nameHint: '',
			cardName: '',
			printings: [],
			printingId: '',
			quantity: 1,
			price: '',
			error: ''
		};
	}

	function addLine() {
		lines = [...lines, blankLine()];
	}
	function removeLine(i: number) {
		lines = lines.filter((_, j) => j !== i);
	}

	async function lookup(line: Line) {
		line.error = '';
		if (!line.setCode.trim() || !line.number.trim()) {
			line.error = 'Set code and number required';
			return;
		}
		try {
			const detail = await api.card(line.setCode.trim(), line.number.trim());
			line.cardName = detail.card.name;
			line.printings = detail.printings;
			line.printingId = detail.printings[0]?.printing_id ?? '';
		} catch (e) {
			line.error = e instanceof Error ? e.message : String(e);
			line.cardName = '';
			line.printings = [];
			line.printingId = '';
		}
	}

	/** Best-effort: pull (quantity, name, price) out of pasted order text. */
	function parse() {
		const re = /^\s*(\d+)\s*x?\s+(.+?)(?:\s*[-–|]\s*)?(?:\$\s*([\d,]+\.?\d*))?\s*$/;
		const parsed: Line[] = [];
		for (const raw of pasteText.split('\n')) {
			const m = re.exec(raw.trim());
			if (!m) continue;
			const line = blankLine();
			line.quantity = Number(m[1]) || 1;
			line.nameHint = m[2].trim();
			line.price = m[3] ? m[3].replace(/,/g, '') : '';
			parsed.push(line);
		}
		if (parsed.length) {
			lines = [...lines, ...parsed];
			pasteText = '';
		}
	}

	const ready = $derived(lines.filter((l) => l.printingId));

	async function commit() {
		if (ready.length === 0) {
			error = 'Resolve at least one line (look up its card) before committing.';
			return;
		}
		committing = true;
		error = null;
		try {
			const detail = await api.createOrder(
				{
					source,
					seller_name: seller.trim() || undefined,
					order_date: orderDate || undefined,
					total: total ? Number(total) : undefined,
					notes: notes.trim() || undefined
				},
				ready.map((l) => ({
					printing_id: l.printingId,
					quantity: l.quantity,
					purchase_price: l.price ? Number(l.price) : null
				}))
			);
			goto(`/orders/${detail.order.id}`);
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			committing = false;
		}
	}
</script>

<svelte:head><title>Import order — PokeDumpster</title></svelte:head>

<h1>Import order</h1>

<section class="meta">
	<label>
		Source
		<select bind:value={source}>
			{#each SOURCES as s (s)}<option value={s}>{s}</option>{/each}
		</select>
	</label>
	<label>Seller <input type="text" bind:value={seller} /></label>
	<label>Order date <input type="date" bind:value={orderDate} /></label>
	<label>Total <input type="number" min="0" step="0.01" bind:value={total} /></label>
	<label class="wide">Notes <input type="text" bind:value={notes} /></label>
</section>

<section class="paste">
	<textarea
		rows="3"
		placeholder="Optionally paste order text (e.g. “2x Charizard ex - $250.00”), then Parse to pre-fill lines."
		bind:value={pasteText}
	></textarea>
	<button class="secondary" onclick={parse} disabled={!pasteText.trim()}>Parse</button>
</section>

<section class="lines">
	<div class="lineshead">
		<h2>Cards ({ready.length}/{lines.length} ready)</h2>
		<button class="secondary" onclick={addLine}>+ Add line</button>
	</div>
	{#if lines.length === 0}
		<p class="muted">Add a line, or paste order text above.</p>
	{/if}
	{#each lines as line, i (i)}
		<div class="line" class:ok={line.printingId}>
			<input class="set" type="text" placeholder="Set" bind:value={line.setCode} />
			<input class="num" type="text" placeholder="#" bind:value={line.number} />
			<button class="secondary" onclick={() => lookup(line)}>Look up</button>
			{#if line.cardName}
				<span class="cardname">{line.cardName}</span>
				<select bind:value={line.printingId}>
					{#each line.printings as p (p.printing_id)}
						<option value={p.printing_id}>{variantLabel(p.variant)}</option>
					{/each}
				</select>
			{:else if line.nameHint}
				<span class="hint">{line.nameHint}</span>
			{:else}
				<span class="hint">—</span>
			{/if}
			<input class="qty" type="number" min="1" bind:value={line.quantity} />
			<input class="price" type="number" min="0" step="0.01" placeholder="$" bind:value={line.price} />
			<button class="link" onclick={() => removeLine(i)}>✕</button>
			{#if line.error}<span class="lineerr">{line.error}</span>{/if}
		</div>
	{/each}
</section>

{#if error}<p class="error">{error}</p>{/if}

<button class="primary" disabled={committing || ready.length === 0} onclick={commit}>
	Commit order ({ready.length} card type(s))
</button>

<style>
	h1,
	h2 {
		color: #e94560;
	}
	h2 {
		font-size: 1rem;
		margin: 0;
	}
	.muted {
		color: #888;
	}
	.error {
		color: #e94560;
	}
	.meta {
		display: flex;
		gap: 1rem;
		flex-wrap: wrap;
		margin: 1rem 0;
	}
	label {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		font-size: 0.8rem;
		color: #888;
	}
	label.wide {
		flex: 1;
		min-width: 200px;
	}
	input,
	select,
	textarea {
		padding: 0.4rem;
		background: #1a1a2e;
		border: 1px solid #0f3460;
		border-radius: 6px;
		color: #e0e0e0;
		font: inherit;
	}
	.paste {
		display: flex;
		gap: 0.5rem;
		align-items: flex-start;
		margin-bottom: 1rem;
	}
	.paste textarea {
		flex: 1;
	}
	.lineshead {
		display: flex;
		justify-content: space-between;
		align-items: center;
		margin-bottom: 0.5rem;
	}
	.line {
		display: flex;
		gap: 0.4rem;
		align-items: center;
		flex-wrap: wrap;
		padding: 0.35rem 0;
		border-bottom: 1px solid #0f3460;
	}
	.line.ok {
		border-left: 3px solid #9fe7a0;
		padding-left: 0.4rem;
	}
	.set {
		width: 80px;
	}
	.num {
		width: 60px;
	}
	.qty {
		width: 56px;
	}
	.price {
		width: 80px;
	}
	.cardname {
		color: #e0e0e0;
		font-weight: 600;
	}
	.hint {
		color: #888;
		font-style: italic;
	}
	.lineerr {
		color: #e94560;
		font-size: 0.8rem;
	}
	button {
		border: none;
		border-radius: 6px;
		cursor: pointer;
		padding: 0.4rem 0.8rem;
		font: inherit;
	}
	button.primary {
		background: #e94560;
		color: #fff;
		margin-top: 1rem;
	}
	button.secondary {
		background: #16213e;
		border: 1px solid #0f3460;
		color: #e0e0e0;
	}
	button.link {
		background: none;
		color: #888;
		padding: 0.2rem 0.4rem;
	}
	button:disabled {
		opacity: 0.5;
		cursor: default;
	}
</style>
