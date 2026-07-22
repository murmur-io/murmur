import { defineConfig } from '@playwright/test';

const port = process.env['MURMUR_E2E_PORT'];
if (!port) {
  throw new Error('MURMUR_E2E_PORT must be assigned by the test harness');
}

export default defineConfig({
  use: { baseURL: `http://localhost:${port}` },
  webServer: {
    command: `npm run start -- --port ${port}`,
    url: `http://localhost:${port}`,
    reuseExistingServer: false,
  },
});
