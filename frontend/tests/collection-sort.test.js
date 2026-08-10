/**
 * The collection list's sort controls name server sort keys, and this is the
 * enforcement.
 *
 * The list renders one bounded page of `/api/collection/search`. Sorting that
 * page in the browser would rank an arbitrary 250 rows and label the result
 * "priciest first" — the answer to a question nobody asked. So the order is the
 * server's, and every control has to be a key the server implements.
 *
 * The two enforcers cannot share code across the language boundary, so they
 * share the list: `pkdump_db::search::ORDER_KEYS` is the canonical set, and
 * this test reads it straight out of the Rust source. Add a sort button whose
 * key the server has never heard of and it lands here, not in production as a
 * column that silently sorts by name.
 *
 * Runs under Node's built-in runner — no test dependency:
 *   npm test        (frontend/)
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

import { SORT_KEYS, SORT_LABELS, defaultDir, isSortKey } from '../src/lib/collectionSort.ts';

const here = dirname(fileURLToPath(import.meta.url));
const searchRs = join(here, '../../crates/pkdump-db/src/search.rs');

/** The `ORDER_KEYS` array literal, read out of the Rust source. */
function serverOrderKeys() {
	const src = readFileSync(searchRs, 'utf8');
	const m = src.match(/pub const ORDER_KEYS: &\[&str\] = &\[([\s\S]*?)\];/);
	assert.ok(m, `ORDER_KEYS not found in ${searchRs} — did it move or get renamed?`);
	return [...m[1].matchAll(/"([^"]+)"/g)].map((x) => x[1]);
}

test('every sort control is a key the server implements', () => {
	const server = new Set(serverOrderKeys());
	assert.ok(server.size > 0, 'parsed no keys out of ORDER_KEYS');
	for (const key of SORT_KEYS) {
		assert.ok(
			server.has(key),
			`sort control "${key}" is not in pkdump_db::search::ORDER_KEYS — ` +
				`the server would fall through to its default sort and the column would lie`
		);
	}
});

test('every sort key has a column label', () => {
	for (const key of SORT_KEYS) {
		assert.equal(typeof SORT_LABELS[key], 'string');
		assert.ok(SORT_LABELS[key].length > 0, `no label for "${key}"`);
	}
});

test('isSortKey rejects anything not offered', () => {
	assert.ok(isSortKey('name'));
	assert.ok(isSortKey('value'));
	// The pre-paging client-side spellings. A stored preference in one of these
	// must fall back rather than be sent to a server that does not know it.
	assert.ok(!isSortKey('type'));
	assert.ok(!isSortKey('nm'));
	assert.ok(!isSortKey('market'));
	assert.ok(!isSortKey(null));
});

test('money and counts start high→low, everything else low→high', () => {
	assert.equal(defaultDir('qty'), 'desc');
	assert.equal(defaultDir('price'), 'desc');
	assert.equal(defaultDir('adjusted'), 'desc');
	assert.equal(defaultDir('value'), 'desc');
	assert.equal(defaultDir('name'), 'asc');
	assert.equal(defaultDir('set'), 'asc');
});
