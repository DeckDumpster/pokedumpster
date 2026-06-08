// Shared builder for "facet" links — clickable card metadata (artist, set,
// rarity, type, Pokédex #, ability, attack, …) that lands on the collection
// page pre-filtered by the proper search DSL. Used by the card detail/modal
// (CardDetailView) and the collection table view so both emit identical,
// correct queries instead of a bare card-name search.
//
// The DSL keywords live in the backend search compiler
// (crates/pkdump-db/src/search.rs); this module only formats them. A value is
// quoted only when the lexer would otherwise split it — it tokenizes an
// unquoted keyword value up to whitespace or ')', so ':', '-', '*', '.' are
// safe bare (see crates/pkdump-core/src/query/lexer.rs::read_value).

/** Format a single `field:value` DSL clause, quoting the value when needed.
 *  Pass `field = ''` for a bare term (an implicit name-contains search). */
export function facetClause(field: string, value: string): string {
	const needsQuote = /[\s()]/.test(value);
	// Card metadata never contains a literal `"`; strip defensively so a stray
	// one can't terminate the quoted run early.
	const v = needsQuote ? `"${value.replace(/"/g, '')}"` : value;
	return field ? `${field}:${v}` : v;
}

/** A `/collection` URL pre-filtered by one facet. Always catalog-wide
 *  (`all=1`) so unowned matches show too — clicking "Mitsuhiro Arita" or a
 *  Pokédex number is an exploration of the whole catalog, not just owned
 *  cards (pokedumpster-67l). */
export function facetHref(field: string, value: string): string {
	const params = new URLSearchParams({ q: facetClause(field, value), all: '1' });
	return `/collection?${params.toString()}`;
}
