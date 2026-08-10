// The collection list's sort controls.
//
// Every key here is a `sort=` value `/api/collection/search` understands —
// `pkdump_db::search::ORDER_KEYS`, canonical spellings. That is not a
// coincidence to be maintained by hand: the list renders one bounded page, so
// re-sorting that page in the browser would rank an arbitrary 250 rows and
// present the answer as if it ranked the result. The server owns the order;
// these buttons only name which one to ask for.
//
// `frontend/tests/collection-sort.test.js` reads ORDER_KEYS out of
// `crates/pkdump-db/src/search.rs` and fails if a key here isn't in it.

export type SortKey =
	| 'name'
	| 'supertype'
	| 'etype'
	| 'rarity'
	| 'set'
	| 'number'
	| 'price'
	| 'adjusted'
	| 'value'
	| 'qty';

export type SortDir = 'asc' | 'desc';

/** Column label per key — what the grid's pills and the table's headers read. */
export const SORT_LABELS: Record<SortKey, string> = {
	name: 'Name',
	supertype: 'Class',
	etype: 'Type',
	rarity: 'Rarity',
	set: 'Set',
	number: '#',
	price: 'NM',
	adjusted: 'Adj.',
	value: 'Value',
	qty: 'Qty'
};

export const SORT_KEYS = Object.keys(SORT_LABELS) as SortKey[];

export function isSortKey(v: string | null): v is SortKey {
	return v !== null && (SORT_KEYS as string[]).includes(v);
}

/**
 * The direction a key starts in when you first pick it. Counts and money read
 * best high→low; everything else low→high.
 */
export function defaultDir(key: SortKey): SortDir {
	return key === 'qty' || key === 'price' || key === 'adjusted' || key === 'value'
		? 'desc'
		: 'asc';
}
