/**
 * Windowing arithmetic for a virtually-scrolled list (pd-7z4o).
 *
 * A list is a stack of BLOCKS laid out in document order with a fixed `gap`
 * between consecutive ones. In the collection table a block is one row; in the
 * grid it is either a section heading or one full row of tiles. The page
 * renders only the blocks that intersect the viewport and stands two spacers
 * in for the rest, so the DOM stays the size of a viewport however many rows
 * matched — which is the whole point of holding the result in JS instead of in
 * the document.
 *
 * Nothing here touches the DOM. Heights come in already measured and the
 * answer goes out as two indices and two spacer heights, which is what makes
 * the geometry testable without a browser.
 */

/**
 * Top edge of every block, plus one past the end.
 *
 * `offsets[i]` is the distance from the top of the list to the top of block
 * `i`. `offsets[n]` is where a block `n` *would* start — i.e. the total height
 * plus one trailing gap — so the total height of the stack is
 * `offsets[n] - gap`.
 *
 * `heightAt` rather than an array of heights: the table stacks 56,635 blocks
 * of one height, and materialising that array to throw it away costs more than
 * the walk does.
 */
export function stackOffsets(
	n: number,
	heightAt: (i: number) => number,
	gap: number
): Float64Array {
	const offsets = new Float64Array(n + 1);
	let y = 0;
	for (let i = 0; i < n; i++) {
		offsets[i] = y;
		y += heightAt(i) + gap;
	}
	offsets[n] = y;
	return offsets;
}

/** The rendered slice, and the two spacers that replace what it leaves out. */
export type Windowed = {
	/** First block rendered. */
	start: number;
	/** One past the last block rendered. */
	end: number;
	/** Spacer standing in for the blocks before `start`; 0 when there are none. */
	padTop: number;
	/** Spacer standing in for the blocks from `end` on; 0 when there are none. */
	padBottom: number;
};

/**
 * The blocks intersecting `[top, bottom)`, and the spacers that leave them at
 * the same y they would have had if every block were rendered.
 *
 * Each spacer is one gap short of the space it replaces, because the layout
 * puts a gap on the spacer's own side too: a top spacer of exactly
 * `offsets[start]` would push the first rendered block one gap too low.
 *
 * At least one block is always returned for a non-empty stack, so a viewport
 * scrolled past the end still renders the tail rather than nothing.
 */
export function windowOf(
	offsets: Float64Array,
	gap: number,
	top: number,
	bottom: number
): Windowed {
	const n = offsets.length - 1;
	if (n <= 0) return { start: 0, end: 0, padTop: 0, padBottom: 0 };
	const start = Math.min(lastAtOrBefore(offsets, n, top), n - 1);
	const end = Math.max(firstAtOrAfter(offsets, n, bottom), start + 1);
	return {
		start,
		end,
		padTop: start === 0 ? 0 : offsets[start] - gap,
		padBottom: end >= n ? 0 : offsets[n] - gap - offsets[end]
	};
}

/** Greatest `i` in `0..=n` with `offsets[i] <= y`; 0 when `y` is above the top. */
function lastAtOrBefore(offsets: Float64Array, n: number, y: number): number {
	let lo = 0;
	let hi = n;
	while (lo < hi) {
		const mid = (lo + hi + 1) >> 1;
		if (offsets[mid] <= y) lo = mid;
		else hi = mid - 1;
	}
	return lo;
}

/** Least `i` in `0..=n` with `offsets[i] >= y`; `n` when `y` is past the end. */
function firstAtOrAfter(offsets: Float64Array, n: number, y: number): number {
	let lo = 0;
	let hi = n;
	while (lo < hi) {
		const mid = (lo + hi) >> 1;
		if (offsets[mid] >= y) hi = mid;
		else lo = mid + 1;
	}
	return lo;
}
