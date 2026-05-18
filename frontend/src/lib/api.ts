// Typed wrappers over the PokeDumpster JSON API. Types are generated from
// the Rust structs by ts-rs (see frontend/src/lib/types/).

import type { CollectionRow } from './types/CollectionRow';
import type { CardDetail } from './types/CardDetail';

async function getJson<T>(url: string): Promise<T> {
	const res = await fetch(url);
	if (!res.ok) {
		throw new Error(`${res.status} ${res.statusText} — ${url}`);
	}
	return (await res.json()) as T;
}

export const api = {
	/** Every copy in the collection, as display rows. */
	collection: () => getJson<CollectionRow[]>('/api/collection'),

	/** Full card detail: the card, its printings, and owned copies. */
	card: (setCode: string, number: string) =>
		getJson<CardDetail>(`/api/card/${encodeURIComponent(setCode)}/${encodeURIComponent(number)}`)
};

/** Turn a variant code (`reverse_holo`) into a label (`Reverse Holo`). */
export function variantLabel(code: string): string {
	return code
		.split('_')
		.map((w) => w.charAt(0).toUpperCase() + w.slice(1))
		.join(' ');
}
