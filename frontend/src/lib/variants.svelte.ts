// Variant display metadata, fetched once from /api/variants and read
// synchronously thereafter. Replaces the ad-hoc label/rank/color/short
// heuristics that used to live in api.ts and a couple of route files —
// the data model owns this, the frontend just renders.

import { api } from './api';
import type { Variant } from './types/Variant';

class VariantsStore {
	map = $state<Record<string, Variant>>({});
	loaded = $state(false);
	private loading: Promise<void> | null = null;

	async load(): Promise<void> {
		if (this.loaded) return;
		if (this.loading) return this.loading;
		this.loading = (async () => {
			const list = await api.variants();
			const m: Record<string, Variant> = {};
			for (const v of list) m[v.code] = v;
			this.map = m;
			this.loaded = true;
		})();
		return this.loading;
	}
}

export const variants = new VariantsStore();

// Fallbacks below are defensive for the pre-load window only; the
// +layout.ts load gate awaits variants.load() before any page renders,
// so in practice the map is always populated when these are called.

/** Human-readable label, e.g. 'pokeball_rh' → 'Poké Ball Reverse Holo'. */
export function variantLabel(code: string): string {
	return variants.map[code]?.label ?? code;
}

/** Sort rank — base treatments first (0), pattern overlays later (3+). */
export function variantRank(code: string): number {
	return variants.map[code]?.rank ?? 100;
}

/** Intra-rank sort key — lower sorts first inside the same rank.
 *  Places first_ed_* before shadowless_* before normal/unlimited_* in
 *  the binder-slot chip ribbon. */
export function variantTiebreak(code: string): number {
	return variants.map[code]?.tiebreak ?? 0;
}

/** Comparator that sorts a list of variant codes by (rank, tiebreak, code). */
export function variantSortCmp(a: string, b: string): number {
	const ra = variantRank(a);
	const rb = variantRank(b);
	if (ra !== rb) return ra - rb;
	const ta = variantTiebreak(a);
	const tb = variantTiebreak(b);
	if (ta !== tb) return ta - tb;
	return a < b ? -1 : a > b ? 1 : 0;
}

/** Chip pip color for browse-slot variant chips. */
export function variantColor(code: string): string {
	return variants.map[code]?.color ?? 'var(--color-chip-fallback)';
}

/** Short tag for the collection table's Variant column ('BALL', 'H', …). */
export function variantTag(code: string): string {
	return variants.map[code]?.short ?? code;
}

/** Origin-of-the-printing description, e.g. "Build & Battle Box" or
 *  "Trick or Trade BOOster Bundle". Null when no canonical source is
 *  known. Rendered as a small subtitle on variant rows. */
export function variantProvenance(code: string): string | null {
	return variants.map[code]?.provenance ?? null;
}
