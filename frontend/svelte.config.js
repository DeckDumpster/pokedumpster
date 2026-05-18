import adapter from '@sveltejs/adapter-static';

/** @type {import('@sveltejs/kit').Config} */
const config = {
	compilerOptions: {
		// Force runes mode for the project, except for libraries.
		runes: ({ filename }) => (filename.split(/[/\\]/).includes('node_modules') ? undefined : true)
	},
	kit: {
		// SPA: a static build with an index.html fallback. The Axum server
		// serves the build and provides the API; there is no Node SSR.
		adapter: adapter({ fallback: 'index.html' })
	}
};

export default config;
