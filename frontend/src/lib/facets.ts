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

/** A `/collection` URL pre-filtered by one facet. The query alone decides the
 *  results, so a click stays in-collection (owned) unless the query the user is
 *  on already carries `is:unowned`, which is kept so a catalog-wide exploration
 *  stays catalog-wide. `currentQuery` is that query (omit it from a card page). */
export function facetHref(field: string, value: string, currentQuery = ''): string {
	const clause = facetClause(field, value);
	const keep = /(^|\s)is:(unowned|missing)(?=\s|$)/i.test(currentQuery);
	const params = new URLSearchParams({ q: keep ? `is:unowned ${clause}` : clause });
	return `/collection?${params.toString()}`;
}
