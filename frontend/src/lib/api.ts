// Typed wrappers over the PokeDumpster JSON API. Types are generated from
// the Rust structs by ts-rs (see frontend/src/lib/types/).

import type { CollectionRow } from './types/CollectionRow';
import type { SearchRow } from './types/SearchRow';
import type { SearchVocabulary } from './types/SearchVocabulary';
import type { CardDetail } from './types/CardDetail';
import type { PriceSeries } from './types/PriceSeries';
import type { CatalogSearchRow } from './types/CatalogSearchRow';
import type { NewCopy } from './types/NewCopy';
import type { CopyEdit } from './types/CopyEdit';
import type { SetSummary } from './types/SetSummary';
import type { SetAnalytics } from './types/SetAnalytics';
import type { BinderPage } from './types/BinderPage';
import type { Binder } from './types/Binder';
import type { BinderDetail } from './types/BinderDetail';
import type { NewBinder } from './types/NewBinder';
import type { BinderEdit } from './types/BinderEdit';
import type { Deck } from './types/Deck';
import type { DeckDetail } from './types/DeckDetail';
import type { NewDeck } from './types/NewDeck';
import type { DeckEdit } from './types/DeckEdit';
import type { SealedEntry } from './types/SealedEntry';
import type { SealedProduct } from './types/SealedProduct';
import type { NewSealed } from './types/NewSealed';
import type { SealedEdit } from './types/SealedEdit';
import type { Order } from './types/Order';
import type { OrderDetail } from './types/OrderDetail';
import type { NewOrder } from './types/NewOrder';
import type { OrderLine } from './types/OrderLine';
import type { WishlistEntry } from './types/WishlistEntry';
import type { NewWish } from './types/NewWish';
import type { WishEdit } from './types/WishEdit';
import type { Batch } from './types/Batch';
import type { BatchDetail } from './types/BatchDetail';
import type { NewBatch } from './types/NewBatch';
import type { ResolutionReport } from './types/ResolutionReport';
import type { CommitResult } from './types/CommitResult';
import type { Variant } from './types/Variant';
import type { ManualPrice } from './types/ManualPrice';
import type { NewManualPrice } from './types/NewManualPrice';
import type { CreateMissingVariant } from './types/CreateMissingVariant';
import type { CreateMissingVariantResult } from './types/CreateMissingVariantResult';
import type { BackupStatus } from './types/BackupStatus';
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

/**
 * A query-language parse error from `/api/collection/search`. `position` is the
 * byte offset into the query where parsing failed, for caret placement.
 */
export class SearchQueryError extends Error {
	position: number;
	constructor(message: string, position: number) {
		super(message);
		this.name = 'SearchQueryError';
		this.position = position;
	}
}

export const api = {
	/** Every copy in the collection, as display rows. */
	collection: () => getJson<CollectionRow[]>('/api/collection'),

	/**
	 * Scryfall-style collection search — one row per printing (owned or not).
	 * An empty `q` returns the default owned view. Throws {@link SearchQueryError}
	 * (with a caret `position`) when the query fails to parse.
	 */
	collectionSearch: async (
		q: string,
		sort?: string,
		dir?: string,
		includeUnowned = false
	): Promise<SearchRow[]> => {
		const params = new URLSearchParams();
		if (q) params.set('q', q);
		if (sort) params.set('sort', sort);
		if (dir) params.set('dir', dir);
		if (includeUnowned) params.set('include_unowned', '1');
		const res = await fetch(`/api/collection/search?${params.toString()}`);
		if (res.status === 400) {
			const body = (await res.json()) as { error: string; position: number };
			throw new SearchQueryError(body.error, body.position);
		}
		if (!res.ok) {
			throw new Error(`${res.status} ${res.statusText} — /api/collection/search`);
		}
		return (await res.json()) as SearchRow[];
	},

	/** The data-driven keyword + flag vocabulary, for autocomplete and help. */
	searchKeywords: () => getJson<SearchVocabulary>('/api/search/keywords'),

	/** Full card detail: the card, its printings, and owned copies. */
	card: (setCode: string, number: string) =>
		getJson<CardDetail>(`/api/card/${encodeURIComponent(setCode)}/${encodeURIComponent(number)}`),

	/** Per-printing market-price time series — drives the card-detail chart. */
	cardPrices: (setCode: string, number: string) =>
		getJson<PriceSeries[]>(
			`/api/card/${encodeURIComponent(setCode)}/${encodeURIComponent(number)}/prices`
		),

	/** Resolve a card name to its newest printing — drives the evolution links. */
	cardByName: (name: string) =>
		getJson<{ set_code: string; number: string }>(
			`/api/cards/by-name/${encodeURIComponent(name)}`
		),

	/** Global catalog search — backs the "All cards" toggle on /collection so
	 *  the user can find and add cards they don't own. */
	cardsCatalog: (q: string, limit = 50) =>
		getJson<CatalogSearchRow[]>(
			`/api/cards/catalog?q=${encodeURIComponent(q)}&limit=${limit}`
		),

	/** Every set + bundle, with card and owned-card counts. Bundles
	 *  carry kind="bundle" so the picker can group them. */
	sets: () => getJson<SetSummary[]>('/api/sets'),

	/** Analytical breakdown for one set: completion, rarity split, value. */
	setAnalytics: (code: string) =>
		getJson<SetAnalytics>(`/api/sets/${encodeURIComponent(code)}/analytics`),

	/** A binder page for a set. */
	binder: (setCode: string, params: Record<string, string | number | boolean>) => {
		const q = new URLSearchParams(Object.entries(params).map(([k, v]) => [k, String(v)]));
		return getJson<BinderPage>(`/api/sets/${encodeURIComponent(setCode)}/binder?${q}`);
	},

	/** Add a copy to the collection; returns the created display row. */
	addCopy: (copy: NewCopyInput) => send<CollectionRow>('POST', '/api/collection', copy),

	/** Delete a collection entry (used to undo an add). */
	deleteCopy: (id: number) => send<void>('DELETE', `/api/collection/${id}`),

	/** Delete many collection entries in one transaction; returns the count. */
	bulkDelete: (ids: number[]) =>
		send<{ deleted: number }>('POST', '/api/collection/bulk-delete', ids),

	/** Delete the most recently added copy of a printing (binder modal "−"). */
	removeCopyByPrinting: (printingId: string) =>
		send<void>('DELETE', `/api/collection/by-printing/${encodeURIComponent(printingId)}`),

	/** Assign a copy to a binder, a deck, or neither (pass empty object). */
	moveCopy: (id: number, body: { binder_id?: number | null; deck_id?: number | null; note?: string }) =>
		send<CollectionRow>('PUT', `/api/collection/${id}/move`, body),

	/** Change a copy's lifecycle status. */
	setCopyStatus: (id: number, status: string, note?: string) =>
		send<CollectionRow>('PUT', `/api/collection/${id}/status`, { status, note }),

	/** Change a copy's printing (correct a mis-logged variant). */
	changePrinting: (id: number, printingId: string) =>
		send<CollectionRow>('PUT', `/api/collection/${id}/printing`, { printing_id: printingId }),

	/** Patch an arbitrary set of editable fields on a copy (condition,
	 *  language, purchase/sale price, notes, tags, grading). Pass only
	 *  the fields you want to change — omitted fields stay as-is. */
	updateCopy: (id: number, edit: Partial<CopyEdit>) =>
		send<CollectionRow>('PUT', `/api/collection/${id}`, edit),

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
	deleteDeck: (id: number) => send<void>('DELETE', `/api/decks/${id}`),

	// --- Sealed products ---
	sealedCollection: () => getJson<SealedEntry[]>('/api/sealed/collection'),
	sealedProducts: (q: string) =>
		getJson<SealedProduct[]>(`/api/sealed/products?q=${encodeURIComponent(q)}`),
	addSealed: (s: Partial<NewSealed> & { product_id: number }) =>
		send<SealedEntry>('POST', '/api/sealed/collection', s),
	updateSealed: (id: number, e: Partial<SealedEdit>) =>
		send<SealedEntry>('PUT', `/api/sealed/collection/${id}`, e),
	deleteSealed: (id: number) => send<void>('DELETE', `/api/sealed/collection/${id}`),

	// --- Orders ---
	orders: () => getJson<Order[]>('/api/orders'),
	orderDetail: (id: number) => getJson<OrderDetail>(`/api/orders/${id}`),
	createOrder: (order: Partial<NewOrder> & { source: string }, lines: OrderLine[]) =>
		send<OrderDetail>('POST', '/api/orders', { order, lines }),
	receiveOrder: (id: number) =>
		send<{ received: number }>('POST', `/api/orders/${id}/receive`),

	// --- Wishlist ---
	wishlist: (includeFulfilled = false) =>
		getJson<WishlistEntry[]>(`/api/wishlist?include_fulfilled=${includeFulfilled}`),
	addWish: (w: Partial<NewWish> & { card_id: string }) => send<number>('POST', '/api/wishlist', w),
	updateWish: (id: number, e: Partial<WishEdit>) => send<void>('PUT', `/api/wishlist/${id}`, e),
	fulfillWish: (id: number, fulfilled: boolean) =>
		send<void>('PUT', `/api/wishlist/${id}/fulfill`, { fulfilled }),
	deleteWish: (id: number) => send<void>('DELETE', `/api/wishlist/${id}`),

	// --- Batches ---
	batches: (limit = 0) => getJson<Batch[]>(`/api/batches${limit ? `?limit=${limit}` : ''}`),
	batchDetail: (id: number) => getJson<BatchDetail>(`/api/batches/${id}`),
	createBatch: (b: Partial<NewBatch> & { batch_type: string }) =>
		send<number>('POST', '/api/batches', b),

	// --- CSV import ---
	importPreview: (format: string, content: string) =>
		send<ResolutionReport>('POST', '/api/import/csv/preview', { format, content }),
	importCommit: (format: string, content: string, name?: string) =>
		send<CommitResult>('POST', '/api/import/csv/commit', { format, content, name }),

	// --- Variants display metadata (backs $lib/variants.svelte) ---
	variants: () => getJson<Variant[]>('/api/variants'),

	// --- Backup freshness (Layer 3 staleness banner, pokedumpster-ivq.5) ---
	/** Off-box backup freshness from the host-side checker's marker. */
	backupStatus: () => getJson<BackupStatus>('/api/backup-status'),

	// --- Manual prices ---
	/** All manual-price entries for one printing, newest first. */
	manualPrices: (printingId: string) =>
		getJson<ManualPrice[]>(
			`/api/manual-prices/by-printing/${encodeURIComponent(printingId)}`
		),
	/** Record a new manual price observation. */
	addManualPrice: (entry: NewManualPrice) =>
		send<number>('POST', '/api/manual-prices', entry),
	/** Delete a manual-price entry. */
	deleteManualPrice: (id: number) =>
		send<void>('DELETE', `/api/manual-prices/${id}`),

	// --- Missing-variant escape hatch ---
	/** Create a user_printing + N copies + optional first manual price. */
	addMissingVariant: (input: CreateMissingVariant) =>
		send<CreateMissingVariantResult>('POST', '/api/user-printings', input),
	/** Remove a user_printing (only if no collection rows reference it). */
	deleteUserPrinting: (printingId: string) =>
		send<void>('DELETE', `/api/user-printings/${encodeURIComponent(printingId)}`),

};
