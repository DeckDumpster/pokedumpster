/**
 * The UI primitives live in dumpster-kit (vendored at the revision `frontend/kit.lock` pins,
 * under `$lib/kit/components`). This re-export keeps routes importing from one place:
 *
 *     import { Panel, Button, Field } from '$lib/components/ui';
 *
 * A route that needs a variant a primitive lacks adds it to the KIT, not here — a primitive
 * declared in this directory would be a second implementation (kit.pin.test.js fails on it).
 */
export * from '$lib/kit/components';
