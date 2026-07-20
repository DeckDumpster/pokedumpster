// Card-condition value multipliers, fetched once from /api/conditions and
// read synchronously thereafter. Replaces the hardcoded multiplier map that
// used to live in conditions.ts — the data model owns these values (they're
// shared with the Rust value-history snapshot), the frontend just reads them.

import { api } from './api';
import type { Condition } from './types/Condition';

class ConditionsStore {
	map = $state<Record<string, Condition>>({});
	loaded = $state(false);
	private loading: Promise<void> | null = null;

	async load(): Promise<void> {
		if (this.loaded) return;
		if (this.loading) return this.loading;
		this.loading = (async () => {
			const list = await api.conditions();
			const m: Record<string, Condition> = {};
			for (const c of list) m[c.name] = c;
			this.map = m;
			this.loaded = true;
		})();
		return this.loading;
	}
}

export const conditions = new ConditionsStore();

/**
 * Standard TCGplayer raw-card price multiplier for a copy's condition. The
 * API's market_price is always the Near-Mint market, so applying this turns
 * it into the copy's realistic value at its recorded condition.
 *
 * Unknown / null defensively defaults to 1.0 (treat as Near Mint) rather than
 * zero, so a typo never silently zeroes a card's displayed value. The
 * +layout.ts load gate awaits conditions.load() before any page renders, so
 * the map is populated whenever this is called.
 */
export function conditionMultiplier(condition: string | null | undefined): number {
	if (condition && condition in conditions.map) {
		return conditions.map[condition].multiplier;
	}
	return 1.0;
}
