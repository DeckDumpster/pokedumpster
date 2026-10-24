/**
 * What the browser does when the sign-in session has lapsed under a page that is
 * already open.
 *
 * The server answers a signed-out browser two ways (dumpster-kit-oidc's layer): a GET is
 * redirected to `/auth/login`, anything else is a 401. A `fetch` from the page follows the
 * first one across origins into Pocket ID and dies on CORS, so a lapsed session used to
 * surface as a bare network error or a 500 shown in place. Every API call goes through
 * `guardedFetch`, which asks for redirects NOT to be followed and recognises both answers.
 */

/** The URL a navigation in flight is heading to, so login returns there, not to the old page. */
let pending: URL | null = null;

export function noteNavigation(to: URL | null): void {
	pending = to;
}

/** The sign-in URL that returns the browser to `target` (path, query and hash). */
export function signInUrl(target: URL): string {
	return `/auth/login?return_to=${encodeURIComponent(target.pathname + target.search + target.hash)}`;
}

/** An expired or missing session, as opposed to any other failure. */
export function isExpiredSession(res: Pick<Response, 'status' | 'type'>): boolean {
	return res.status === 401 || res.type === 'opaqueredirect';
}

/** Send the browser to sign in; resolves never, because the page is going away. */
export function toSignIn(current: URL = new URL(location.href)): Promise<never> {
	location.assign(signInUrl(pending ?? current));
	return new Promise<never>(() => {});
}

/** `fetch` that sends an expired session to sign-in instead of returning an error. */
export async function guardedFetch(input: string, init: RequestInit = {}): Promise<Response> {
	const res = await fetch(input, { ...init, redirect: 'manual' });
	if (isExpiredSession(res)) return toSignIn();
	return res;
}
