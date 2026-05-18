// Typed wrappers over the PokeDumpster JSON API. Types are generated from
// the Rust structs by ts-rs (see frontend/src/lib/types/).

import type { CollectionRow } from './types/CollectionRow';
import type { CardDetail } from './types/CardDetail';
import type { NewCopy } from './types/NewCopy';
import type { SetSummary } from './types/SetSummary';
import type { BinderPage } from './types/BinderPage';

async function getJson<T>(url: string): Promise<T> {
	const res = await fetch(url);
	if (!res.ok) {
		throw new Error(`${res.status} ${res.statusText} — ${url}`);
	}
	return (await res.json()) as T;
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
		const q = new URLSearchParams(
			Object.entries(params).map(([k, v]) => [k, String(v)])
		);
		return getJson<BinderPage>(`/api/sets/${encodeURIComponent(setCode)}/binder?${q}`);
	},

	/** Add a copy to the collection; returns the created display row. */
	async addCopy(copy: NewCopyInput): Promise<CollectionRow> {
		const res = await fetch('/api/collection', {
			method: 'POST',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify(copy)
		});
		if (!res.ok) {
			throw new Error(`${res.status} ${res.statusText} — POST /api/collection`);
		}
		return (await res.json()) as CollectionRow;
	}
};

/** Turn a variant code (`reverse_holo`) into a label (`Reverse Holo`). */
export function variantLabel(code: string): string {
	return code
		.split('_')
		.map((w) => w.charAt(0).toUpperCase() + w.slice(1))
		.join(' ');
}
