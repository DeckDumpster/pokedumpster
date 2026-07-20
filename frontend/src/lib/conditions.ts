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

// The value multipliers for these conditions are DATA, not code — they live
// in the `conditions` table (seeded from data/conditions.json) and are read
// via `conditionMultiplier` from `$lib/conditions.svelte`, sharing one source
// with the Rust value-history snapshot. See pokedumpster-e1vo.
