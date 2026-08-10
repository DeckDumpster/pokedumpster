<!--
	Pager — moving through a result set the server hands over one page at a
	time.

	Built for /collection, where the whole result stopped fitting: a
	catalog-wide query matches 56,635 printings, and rendering them all put
	56,635 tiles in the DOM and killed the tab (pd-tsqd). The endpoint answers
	with one bounded page (pd-jsby), so this is the control that reaches the
	rest of it.

	The page SIZE is not this component's to choose, and not its caller's
	either: `limit` is whatever the endpoint said it served, echoed back off
	the response. That is also why the control speaks in `offset` rather than
	page number — an offset needs no agreement about how big a page is, which
	means a link to one survives the server changing its mind.

	Renders nothing when the whole result is already on screen. A pager for a
	single page is a control that can only tell you where you already are.
-->
<script lang="ts">
	import type { HTMLAttributes } from 'svelte/elements';
	import { count } from '$lib/format';

	type Props = {
		/** Rows skipped to reach the page on screen. */
		offset: number;
		/** Rows on a page — the endpoint's own bound, echoed back. */
		limit: number;
		/** Rows the query matches in total, ignoring paging. */
		total: number;
		/** What each row is called, for the "1–250 of 56,635 cards" line. */
		unit?: string;
		/** Asked to move; the caller re-fetches and re-renders at `offset`. */
		ongo?: (offset: number) => void;
		class?: string;
	} & HTMLAttributes<HTMLDivElement>;

	let {
		offset,
		limit,
		total,
		unit = 'results',
		ongo = undefined,
		class: extra = '',
		...rest
	}: Props = $props();

	// A limit of 0 would divide by zero; treat it as "no page served yet".
	const pages = $derived(limit > 0 ? Math.ceil(total / limit) : 0);
	const current = $derived(limit > 0 ? Math.floor(offset / limit) + 1 : 1);
	const first = $derived(total === 0 ? 0 : offset + 1);
	const last = $derived(Math.min(offset + limit, total));
	const hasPrev = $derived(offset > 0);
	const hasNext = $derived(offset + limit < total);
</script>

{#if pages > 1}
	<div class={['pager', extra].filter(Boolean).join(' ')} role="navigation" aria-label="Pages" {...rest}>
		<button
			class="step"
			type="button"
			data-testid="pager-prev"
			disabled={!hasPrev}
			onclick={() => ongo?.(Math.max(0, offset - limit))}
		>
			‹ Prev
		</button>
		<span class="where" data-testid="pager-position">
			<span class="page">Page {count(current)} of {count(pages)}</span>
			<span class="range">{count(first)}–{count(last)} of {count(total)} {unit}</span>
		</span>
		<button
			class="step"
			type="button"
			data-testid="pager-next"
			disabled={!hasNext}
			onclick={() => ongo?.(offset + limit)}
		>
			Next ›
		</button>
	</div>
{/if}

<style>
	.pager {
		display: flex;
		align-items: center;
		justify-content: center;
		flex-wrap: wrap;
		gap: var(--space-3);
		margin: var(--space-5) var(--space-0);
	}
	.step {
		background: none;
		border: 1px solid var(--color-border);
		border-radius: var(--radius-md);
		color: var(--color-text-subtle);
		font: inherit;
		font-size: var(--text-md);
		line-height: var(--leading-tight);
		padding: var(--space-1) var(--space-3);
		white-space: nowrap;
		cursor: pointer;
		transition:
			border-color var(--dur-fast) var(--ease-standard),
			color var(--dur-fast) var(--ease-standard);
	}
	.step:hover:not(:disabled) {
		border-color: var(--color-border-accent);
		color: var(--color-text);
	}
	.step:focus-visible {
		outline: none;
		box-shadow: var(--shadow-focus);
	}
	.step:disabled {
		opacity: 0.4;
		cursor: default;
	}
	/* Two lines, because they answer two different questions: which page you
	   are on, and how much of the result that is. */
	.where {
		display: inline-flex;
		flex-direction: column;
		align-items: center;
		gap: var(--space-0-5);
		text-align: center;
	}
	.page {
		color: var(--color-text);
		font-size: var(--text-md);
		font-weight: var(--weight-medium);
	}
	.range {
		color: var(--color-text-subtle);
		font-size: var(--text-xs);
		font-variant-numeric: tabular-nums;
	}
</style>
