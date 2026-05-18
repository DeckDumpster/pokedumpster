import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [sveltekit()],
	server: {
		// In `npm run dev`, proxy API calls to the Axum server so the SPA
		// and API share an origin (as they do in production).
		proxy: {
			'/api': 'http://localhost:8080',
			'/health': 'http://localhost:8080'
		}
	}
});
