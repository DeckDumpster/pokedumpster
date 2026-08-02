import { test } from 'node:test';
import assert from 'node:assert/strict';
import EmptyState from '../../src/lib/components/ui/EmptyState.svelte';
import { markup, styles, rootClass, slot, assertTokenOnly, assertTokensDeclared } from '../support/render.js';

/** @param {Record<string, unknown>} [props] */
const body = (props = {}) => markup(EmptyState, { title: 'No decks yet.', ...props });

test('EmptyState renders its message', () => {
	const html = body();
	assert.match(html, /No decks yet\./);
	assert.match(rootClass(html), /\bempty\b/);
});

test('EmptyState respects its variants', () => {
	assert.match(rootClass(body({ tone: 'success' })), /\bt-success\b/);
	assert.match(rootClass(body()), /\bt-neutral\b/);
	assert.match(rootClass(body({ size: 'sm' })), /\bs-sm\b/);
	assert.match(rootClass(body()), /\bs-md\b/);
});

test('EmptyState draws the description and action only when given them', () => {
	const bare = body();
	assert.doesNotMatch(bare, /class="description/);
	assert.doesNotMatch(bare, /class="action/);

	const full = body({
		description: 'Create one to group cards for play.',
		action: slot('<button>New deck</button>')
	});
	assert.match(full, /class="description/);
	assert.match(full, /Create one to group cards for play\./);
	assert.match(full, /<button>New deck<\/button>/);
});

test('EmptyState emits no hardcoded colour', () => {
	assertTokenOnly('EmptyState.svelte');
	const css = styles(EmptyState, { title: 'x' });
	// Empty is sometimes the GOOD outcome (/ingest/unresolved's cleared queue),
	// and that case has to be able to say so.
	assert.match(css, /\.t-success[^{]*\{[^}]*var\(--color-success-text\)/);
	assertTokensDeclared(css);
});
