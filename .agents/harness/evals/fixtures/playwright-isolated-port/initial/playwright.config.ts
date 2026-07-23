import { defineConfig } from '@playwright/test';

export default defineConfig({
  use: { baseURL: 'http://localhost:4210' },
  webServer: {
    command: 'npm run start -- --port 4210',
    url: 'http://localhost:4210',
    reuseExistingServer: true,
  },
});
