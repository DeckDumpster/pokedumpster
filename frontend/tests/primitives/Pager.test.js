import { test } from 'node:test';
import assert from 'node:assert/strict';
import Pager from '../../src/lib/components/ui/Pager.svelte';
import { markup, styles, assertTokenOnly, assertTokensDeclared } from '../support/render.js';

/** The reported case: `q=supertype:Pokémon&all=1` on the real catalog. */
const HUGE = { offset: 0, limit: 250, total: 56635, unit: 'cards' };

test('Pager renders nothing when the whole result is already on screen', () => {
	// A pager for one page can only tell you where you already are. Svelte's
	// server renderer still emits its hydration markers, so "nothing" is
	// "no element", not an empty string.
	const nothing = (/** @type {string} */ html) =>
		assert.doesNotMatch(html, /<\w/, `expected no markup, got: ${html}`);
	nothing(markup(Pager, { offset: 0, limit: 250, total: 250 }));
	nothing(markup(Pager, { offset: 0, limit: 250, total: 0 }));
	// A limit of 0 is "no page served yet", not a division by zero.
	nothing(markup(Pager, { offset: 0, limit: 0, total: 56635 }));
});

test('Pager states which page of how many, and which rows those are', () => {
	const html = markup(Pager, HUGE);
	assert.match(html, /Page 1 of 227/);
	assert.match(html, /1–250 of 56,635 cards/);

	const mid = markup(Pager, { ...HUGE, offset: 500 });
	assert.match(mid, /Page 3 of 227/);
	assert.match(mid, /501–750 of 56,635 cards/);
});

test('Pager clamps its last page to the total', () => {
	// 56,635 is not a multiple of 250: the final page is short and must say so
	// rather than claim rows the result does not have.
	const end = markup(Pager, { ...HUGE, offset: 56500 });
	assert.match(end, /Page 227 of 227/);
	assert.match(end, /56,501–56,635 of 56,635 cards/);
});

test('Pager disables the step that would leave the result', () => {
	const first = markup(Pager, HUGE);
	assert.match(first, /data-testid="pager-prev"[^>]*disabled/);
	assert.doesNotMatch(first, /data-testid="pager-next"[^>]*disabled/);

	const last = markup(Pager, { ...HUGE, offset: 56500 });
	assert.doesNotMatch(last, /data-testid="pager-prev"[^>]*disabled/);
	assert.match(last, /data-testid="pager-next"[^>]*disabled/);
});

test('Pager is a labelled navigation landmark', () => {
	const html = markup(Pager, HUGE);
	assert.match(html, /role="navigation"/);
	assert.match(html, /aria-label="Pages"/);
});

test('Pager names its visual values through semantic tokens only', () => {
	assertTokenOnly('Pager.svelte');
	assertTokensDeclared(styles(Pager, HUGE));
});
