// Typed wrappers over the PokeDumpster JSON API. Types are generated from
// the Rust structs by ts-rs (see frontend/src/lib/types/).

import type { CollectionRow } from './types/CollectionRow';
import type { CardDetail } from './types/CardDetail';
import type { NewCopy } from './types/NewCopy';
import type { SetSummary } from './types/SetSummary';
import type { BinderPage } from './types/BinderPage';
import type { Binder } from './types/Binder';
import type { BinderDetail } from './types/BinderDetail';
import type { NewBinder } from './types/NewBinder';
import type { BinderEdit } from './types/BinderEdit';
import type { Deck } from './types/Deck';
import type { DeckDetail } from './types/DeckDetail';
import type { NewDeck } from './types/NewDeck';
import type { DeckEdit } from './types/DeckEdit';

async function getJson<T>(url: string): Promise<T> {
	const res = await fetch(url);
	if (!res.ok) {
		throw new Error(`${res.status} ${res.statusText} — ${url}`);
	}
	return (await res.json()) as T;
}

/** POST/PUT/DELETE with an optional JSON body. 204 responses yield undefined. */
async function send<T>(method: string, url: string, body?: unknown): Promise<T> {
	const res = await fetch(url, {
		method,
		headers: body !== undefined ? { 'content-type': 'application/json' } : {},
		body: body !== undefined ? JSON.stringify(body) : undefined
	});
	if (!res.ok) {
		throw new Error(`${res.status} ${res.statusText} — ${method} ${url}`);
	}
	return (res.status === 204 ? undefined : await res.json()) as T;
}

/** A NewCopy with only the fields the caller cares to set. */
export type NewCopyInput = Partial<NewCopy> & { printing_id: string; source: string };

export const api = {
	/** Every copy in the collection, as display rows. */
	collection: () => getJson<CollectionRow[]>('/api/collection'),

	/** Full card detail: the card, its printings, and owned copies. */
	card: (setCode: string, number: string) =>
		getJson<CardDetail>(`/api/card/${encodeURIComponent(setCode)}/${encodeURIComponent(number)}`),

	/** Every set, with card and owned-card counts. */
	sets: () => getJson<SetSummary[]>('/api/sets'),

	/** A binder page for a set. */
	binder: (setCode: string, params: Record<string, string | number | boolean>) => {
		const q = new URLSearchParams(Object.entries(params).map(([k, v]) => [k, String(v)]));
		return getJson<BinderPage>(`/api/sets/${encodeURIComponent(setCode)}/binder?${q}`);
	},

	/** Add a copy to the collection; returns the created display row. */
	addCopy: (copy: NewCopyInput) => send<CollectionRow>('POST', '/api/collection', copy),

	/** Delete a collection entry (used to undo an add). */
	deleteCopy: (id: number) => send<void>('DELETE', `/api/collection/${id}`),

	/** Assign a copy to a binder, a deck, or neither (pass empty object). */
	moveCopy: (id: number, body: { binder_id?: number | null; deck_id?: number | null; note?: string }) =>
		send<CollectionRow>('PUT', `/api/collection/${id}/move`, body),

	// --- Binders ---
	binders: () => getJson<Binder[]>('/api/binders'),
	binderDetail: (id: number) => getJson<BinderDetail>(`/api/binders/${id}`),
	createBinder: (b: Partial<NewBinder> & { name: string }) =>
		send<Binder>('POST', '/api/binders', b),
	updateBinder: (id: number, e: Partial<BinderEdit>) =>
		send<Binder>('PUT', `/api/binders/${id}`, e),
	deleteBinder: (id: number) => send<void>('DELETE', `/api/binders/${id}`),

	// --- Decks ---
	decks: () => getJson<Deck[]>('/api/decks'),
	deckDetail: (id: number) => getJson<DeckDetail>(`/api/decks/${id}`),
	createDeck: (d: Partial<NewDeck> & { name: string }) => send<Deck>('POST', '/api/decks', d),
	updateDeck: (id: number, e: Partial<DeckEdit>) => send<Deck>('PUT', `/api/decks/${id}`, e),
	deleteDeck: (id: number) => send<void>('DELETE', `/api/decks/${id}`)
};

/** Turn a variant code (`reverse_holo`) into a label (`Reverse Holo`). */
export function variantLabel(code: string): string {
	return code
		.split('_')
		.map((w) => w.charAt(0).toUpperCase() + w.slice(1))
		.join(' ');
}
