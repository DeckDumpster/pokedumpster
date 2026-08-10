/**
 * The virtual scroller's geometry (pd-7z4o).
 *
 * The browser tier proves the DOM stays bounded; this proves the arithmetic
 * that bounds it. The property that matters in every case below is the same
 * one: rendering the window and its two spacers must occupy exactly the height
 * the whole stack would have, and must leave the first rendered block at
 * exactly the y it would have had — otherwise the list shifts under the reader
 * as they scroll.
 */

import test from 'node:test';
import assert from 'node:assert/strict';
import { stackOffsets, windowOf } from '$lib/virtual';

/** Uniform heights, the table's case.
    @type {(n: number, h: number, gap?: number) => Float64Array} */
const uniform = (n, h, gap = 0) => stackOffsets(n, () => h, gap);

/**
 * What the laid-out document actually measures, given a window: the top
 * spacer, then the rendered blocks with a gap between every pair of adjacent
 * items (spacers included), then the bottom spacer.
 *
 * @type {(offsets: Float64Array, gap: number,
 *         w: import('$lib/virtual').Windowed,
 *         heightAt: (i: number) => number) => {total: number, firstTop: number}}
 */
function laidOut(offsets, gap, w, heightAt) {
	/** @type {number[]} */
	const items = [];
	if (w.padTop > 0) items.push(w.padTop);
	for (let i = w.start; i < w.end; i++) items.push(heightAt(i));
	if (w.padBottom > 0) items.push(w.padBottom);
	const total = items.reduce((a, b) => a + b, 0) + Math.max(0, items.length - 1) * gap;
	// Where the first rendered block lands: after the top spacer and its gap.
	const firstTop = w.padTop > 0 ? w.padTop + gap : 0;
	return { total, firstTop };
}

test('offsets stack heights and gaps, and the last entry carries a trailing gap', () => {
	const o = uniform(4, 10, 2);
	assert.deepEqual([...o], [0, 12, 24, 36, 48]);
	// Total height is one gap less than the final offset — there is no gap
	// after the last block.
	assert.equal(o[4] - 2, 46);
});

test('a mixed stack offsets each block by what precedes it', () => {
	// header, tiles, tiles, header, tiles — the grid's shape.
	const h = [30, 210, 210, 30, 210];
	const o = stackOffsets(5, (/** @type {number} */ i) => h[i], 16);
	assert.deepEqual([...o], [0, 46, 272, 498, 544, 770]);
});

test('an empty stack has an empty window', () => {
	const w = windowOf(stackOffsets(0, () => 10, 4), 4, 0, 800);
	assert.deepEqual(w, { start: 0, end: 0, padTop: 0, padBottom: 0 });
});

test('the top of the list renders from block zero with no top spacer', () => {
	const o = uniform(1000, 48);
	const w = windowOf(o, 0, 0, 480);
	assert.equal(w.start, 0);
	assert.equal(w.padTop, 0);
	// 480 / 48 — the first block outside the viewport is 10.
	assert.equal(w.end, 10);
	assert.equal(w.padBottom, 48 * 990);
});

test('a window in the middle keeps the stack the same height and the same place', () => {
	const o = uniform(1000, 48);
	const w = windowOf(o, 0, 5000, 5800);
	const { total, firstTop } = laidOut(o, 0, w, () => 48);
	assert.equal(total, 48 * 1000, 'the document is still the height of the whole list');
	assert.equal(firstTop, o[w.start], 'the first rendered block is where it belongs');
	// And it really is a window, not the list.
	assert.ok(w.end - w.start < 30, `rendered ${w.end - w.start} blocks`);
});

test('gaps are accounted for on both sides of both spacers', () => {
	const gap = 16;
	const h = (/** @type {number} */ i) => (i % 4 === 0 ? 30 : 210);
	const o = stackOffsets(500, h, gap);
	const w = windowOf(o, gap, 12_000, 12_900);
	const { total, firstTop } = laidOut(o, gap, w, h);
	assert.equal(total, o[500] - gap, 'the document is the height of the whole stack');
	assert.equal(firstTop, o[w.start], 'the first rendered block is where it belongs');
});

test('the last window has no bottom spacer and still measures the whole stack', () => {
	const gap = 16;
	const o = uniform(200, 210, gap);
	const w = windowOf(o, gap, o[200] - gap - 400, o[200]);
	assert.equal(w.end, 200);
	assert.equal(w.padBottom, 0);
	const { total, firstTop } = laidOut(o, gap, w, () => 210);
	assert.equal(total, o[200] - gap);
	assert.equal(firstTop, o[w.start]);
});

test('a viewport scrolled past the end still renders the tail, never nothing', () => {
	const o = uniform(100, 48);
	const w = windowOf(o, 0, 99_000, 99_800);
	assert.equal(w.start, 99);
	assert.equal(w.end, 100);
});

test('a viewport above the list renders from the top', () => {
	const o = uniform(100, 48);
	const w = windowOf(o, 0, -500, 300);
	assert.equal(w.start, 0);
	assert.equal(w.padTop, 0);
});

test('the rendered count is bounded by the viewport, not by the result size', () => {
	// The bead's single assertion, in arithmetic: ten times the rows, same
	// number of nodes.
	const small = windowOf(uniform(1_000, 48), 0, 20_000, 20_900);
	const huge = windowOf(uniform(100_000, 48), 0, 20_000, 20_900);
	assert.equal(huge.end - huge.start, small.end - small.start);
});
