/**
 * An expired sign-in session on an open page goes to login and comes back (sp-9jn30t).
 */
import test from 'node:test';
import assert from 'node:assert/strict';
import { signInUrl, isExpiredSession, guardedFetch, noteNavigation } from '$lib/session';

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
	const assigned = [];
	globalThis.location = { href: 'https://x.test/sets/base1', assign: (u) => assigned.push(u) };
	let seen;
	globalThis.fetch = async (_u, init) => {
		seen = init;
		return { status: 0, type: 'opaqueredirect' };
	};
	void guardedFetch('/api/sets'); // never resolves: the page is leaving
	await new Promise((r) => setTimeout(r, 5));
	assert.equal(seen.redirect, 'manual');
	assert.deepEqual(assigned, ['/auth/login?return_to=%2Fsets%2Fbase1']);

	noteNavigation(new URL('https://x.test/card/abc'));
	globalThis.fetch = async () => ({ status: 401, type: 'basic' });
	void guardedFetch('/api/x', { method: 'POST' });
	await new Promise((r) => setTimeout(r, 5));
	assert.equal(assigned[1], '/auth/login?return_to=%2Fcard%2Fabc');

	noteNavigation(null);
	const boom = { status: 500, type: 'basic' };
	globalThis.fetch = async () => boom;
	assert.equal(await guardedFetch('/api/x'), boom);
	assert.equal(assigned.length, 2);
});
