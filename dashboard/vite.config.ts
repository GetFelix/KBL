// The page calls the tracker at its own origin (`/v1/...`), so in development Vite
// forwards those calls to the tracker named by KBL_TRACKER_URL.
import { defineConfig } from 'vite';

const tracker = process.env.KBL_TRACKER_URL;

export default defineConfig({
  server: tracker ? { proxy: { '/v1': { target: tracker, changeOrigin: true } } } : {},
});
