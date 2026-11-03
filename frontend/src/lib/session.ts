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

/**
 * The loop guard (sp-xnr4k9). Pocket ID signs a browser that still holds its own session straight
 * back in, so if the server refuses a session it has just issued, "send them to sign in" turns
 * into an endless redirect loop. The time of the last trip to sign-in is kept in sessionStorage
 * (it survives the round trip through the provider in the same tab); an expired session seen
 * within `SIGN_IN_LOOP_MS` of it is a fault, shown as an error, not another trip.
 */
export const SIGN_IN_LOOP_MS = 60_000;
const SIGNED_IN_AT = 'pkdump.sign-in-sent-at';

export class SignInLoopError extends Error {
	constructor() {
		super(
			'You just signed in, but the server refused the new session. Not sending you back to ' +
				'sign in again, which would loop. Reload to retry; if it persists the server is misconfigured.'
		);
		this.name = 'SignInLoopError';
	}
}

function store(): Storage | null {
	try {
		return typeof sessionStorage === 'undefined' ? null : sessionStorage;
	} catch {
		return null;
	}
}

/** Whether a trip to sign-in was sent so recently that another one would be a loop. */
export function justSignedIn(now: number = Date.now()): boolean {
	const at = Number(store()?.getItem(SIGNED_IN_AT) ?? NaN);
	return Number.isFinite(at) && now - at >= 0 && now - at < SIGN_IN_LOOP_MS;
}

/** Send the browser to sign in; resolves never, because the page is going away. */
export function toSignIn(current: URL = new URL(location.href)): Promise<never> {
	try {
		store()?.setItem(SIGNED_IN_AT, String(Date.now()));
	} catch {
		// No storage: no loop guard, but sign-in still works.
	}
	location.assign(signInUrl(pending ?? current));
	return new Promise<never>(() => {});
}

/**
 * `fetch` that sends an expired session to sign-in instead of returning an error — unless it
 * just did, in which case it throws `SignInLoopError` for the page to show.
 */
export async function guardedFetch(input: string, init: RequestInit = {}): Promise<Response> {
	const res = await fetch(input, { ...init, redirect: 'manual' });
	if (isExpiredSession(res)) {
		if (justSignedIn()) throw new SignInLoopError();
		return toSignIn();
	}
	return res;
}
