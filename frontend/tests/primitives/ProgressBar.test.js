import { test } from 'node:test';
import assert from 'node:assert/strict';
import ProgressBar from '../../src/lib/components/ui/ProgressBar.svelte';
import { markup, styles, rootClass, assertTokenOnly, assertTokensDeclared } from '../support/render.js';

/** @param {Record<string, unknown>} [props] */
const body = (props = {}) => markup(ProgressBar, { value: 0, max: 100, ...props });
/** @param {string} html */
const width = (html) => /width:\s*([0-9.]+)%/.exec(html)?.[1];

test('ProgressBar renders a track filled to the value', () => {
	const html = body({ value: 42, max: 102 });
	assert.match(rootClass(html), /\bbar\b/);
	assert.equal(Number(width(html)).toFixed(2), ((42 / 102) * 100).toFixed(2));
});

test('ProgressBar respects its variants', () => {
	for (const tone of ['accent', 'complete', 'partial', 'empty']) {
		assert.match(rootClass(body({ tone })), new RegExp(`\\bt-${tone}\\b`));
	}
	assert.match(rootClass(body()), /\bt-accent\b/, 'tone defaults to the brand fill /browse uses');
	assert.match(rootClass(body({ size: 'md' })), /\bs-md\b/);
	assert.match(rootClass(body()), /\bs-sm\b/);
});

test('ProgressBar clamps rather than overflowing', () => {
	assert.equal(width(body({ value: 200, max: 100 })), '100');
	assert.equal(width(body({ value: -5, max: 100 })), '0');
	// A synthesized set with no cards yet: 0/0 is 0%, not NaN%.
	assert.equal(width(body({ value: 0, max: 0 })), '0');
});

test('ProgressBar is a progressbar to assistive tech', () => {
	const html = body({ value: 42, max: 102, label: 'Base 42/102' });
	assert.match(html, /role="progressbar"/);
	assert.match(html, /aria-valuenow="42"/);
	assert.match(html, /aria-valuemax="102"/);
	assert.match(html, /aria-valuemin="0"/);
	assert.match(html, /aria-label="Base 42\/102"/);
	assert.match(html, /Base 42\/102/, 'the label is also drawn as the caption');
});

test('ProgressBar emits no hardcoded colour', () => {
	assertTokenOnly('ProgressBar.svelte');
	const css = styles(ProgressBar, { value: 1, max: 2 });
	assert.match(css, /background:\s*var\(--color-progress-track\)/);
	assertTokensDeclared(css);
});
