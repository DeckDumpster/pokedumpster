// The five condition tiers enforced by the `collection.condition` CHECK
// constraint (crates/pkdump-db/src/schema_user.sql). Anything else gets
// rejected at the DB layer, so the UI must pick from this list.

export const CONDITIONS = [
	'Near Mint',
	'Lightly Played',
	'Moderately Played',
	'Heavily Played',
	'Damaged'
] as const;

export type Condition = (typeof CONDITIONS)[number];

// Standard TCGplayer raw-card price multipliers. Used wherever the UI
// renders the estimated value of an *owned copy* — the API's
// market_price is always the NM market, so applying this multiplier
// turns it into the realistic value of the copy at its recorded
// condition. Per-printing prices (e.g. the binder modal stepper rows)
// do NOT use this — those represent printings in the abstract, not
// specific copies.
const CONDITION_MULTIPLIERS: Record<string, number> = {
	'Near Mint': 1.0,
	'Lightly Played': 0.85,
	'Moderately Played': 0.65,
	'Heavily Played': 0.45,
	Damaged: 0.25
};

export function conditionMultiplier(condition: string | null | undefined): number {
	if (condition && condition in CONDITION_MULTIPLIERS) {
		return CONDITION_MULTIPLIERS[condition];
	}
	// Unknown / null defensive default — treat as NM rather than zero so
	// a typo doesn't silently zero out a card's displayed value.
	return 1.0;
}
