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
