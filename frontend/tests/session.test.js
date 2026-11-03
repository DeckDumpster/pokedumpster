/**
 * An expired sign-in session on an open page goes to login and comes back (sp-9jn30t).
 */
import test from 'node:test';
import assert from 'node:assert/strict';
import {
	signInUrl,
	isExpiredSession,
	guardedFetch,
	noteNavigation,
	SignInLoopError,
	SIGN_IN_LOOP_MS
} from '$lib/session';

test('login returns to the deep page, query and hash included', () => {
	const u = signInUrl(new URL('https://x.test/collection?q=a%20b&page=2#row-9'));
	assert.equal(u, '/auth/login?return_to=%2Fcollection%3Fq%3Da%2520b%26page%3D2%23row-9');
});

test('a 401 and an unfollowed redirect are an expired session; a 500 is not', () => {
	assert.ok(isExpiredSession({ status: 401, type: 'basic' }));
	assert.ok(isExpiredSession({ status: 0, type: 'opaqueredirect' }));
	assert.ok(!isExpiredSession({ status: 500, type: 'basic' }));
	assert.ok(!isExpiredSession({ status: 200, type: 'basic' }));
});

test('guardedFetch navigates to login for an expired session and passes errors through', async () => {
	delete (/** @type {any} */ (globalThis).sessionStorage); // no storage: no loop guard
	/** @type {string[]} */
	const assigned = [];
	/** @type {any} */ (globalThis).location = {
		href: 'https://x.test/sets/base1',
		assign: (/** @type {string} */ u) => assigned.push(u)
	};
	/** @type {RequestInit | undefined} */
	let seen;
	/** @type {any} */ (globalThis).fetch = async (/** @type {unknown} */ _u, /** @type {RequestInit} */ init) => {
		seen = init;
		return { status: 0, type: 'opaqueredirect' };
	};
	void guardedFetch('/api/sets'); // never resolves: the page is leaving
	await new Promise((r) => setTimeout(r, 5));
	assert.equal(seen?.redirect, 'manual');
	assert.deepEqual(assigned, ['/auth/login?return_to=%2Fsets%2Fbase1']);

	noteNavigation(new URL('https://x.test/card/abc'));
	/** @type {any} */ (globalThis).fetch = async () => ({ status: 401, type: 'basic' });
	void guardedFetch('/api/x', { method: 'POST' });
	await new Promise((r) => setTimeout(r, 5));
	assert.equal(assigned[1], '/auth/login?return_to=%2Fcard%2Fabc');

	noteNavigation(null);
	const boom = { status: 500, type: 'basic' };
	/** @type {any} */ (globalThis).fetch = async () => boom;
	assert.equal(await guardedFetch('/api/x'), boom);
	assert.equal(assigned.length, 2);
});

/** A sessionStorage stand-in. */
function memoryStorage() {
	/** @type {Map<string, string>} */
	const m = new Map();
	return {
		getItem: (/** @type {string} */ k) => m.get(k) ?? null,
		setItem: (/** @type {string} */ k, /** @type {string} */ v) => void m.set(k, v)
	};
}

test('a 401 right after a fresh sign-in is an error, not another trip to sign-in (sp-xnr4k9)', async () => {
	/** @type {string[]} */
	const assigned = [];
	/** @type {any} */ (globalThis).sessionStorage = memoryStorage();
	/** @type {any} */ (globalThis).location = {
		href: 'https://x.test/collection',
		assign: (/** @type {string} */ u) => assigned.push(u)
	};
	/** @type {any} */ (globalThis).fetch = async () => ({ status: 401, type: 'basic' });

	// First expired session: off to sign in, as before.
	void guardedFetch('/api/collection');
	await new Promise((r) => setTimeout(r, 5));
	assert.equal(assigned.length, 1);

	// Back from Pocket ID, and the server still says 401: shown, not looped.
	await assert.rejects(guardedFetch('/api/collection'), SignInLoopError);
	assert.equal(assigned.length, 1, 'sent back to sign-in again: the loop');

	// Long after that trip, an expired session is a real expiry again.
	/** @type {any} */ (globalThis).sessionStorage.setItem(
		'pkdump.sign-in-sent-at',
		String(Date.now() - SIGN_IN_LOOP_MS - 1)
	);
	void guardedFetch('/api/collection');
	await new Promise((r) => setTimeout(r, 5));
	assert.equal(assigned.length, 2);
	delete (/** @type {any} */ (globalThis).sessionStorage);
});
