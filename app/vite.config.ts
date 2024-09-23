import preact from '@preact/preset-vite';
import tailwindcss from '@tailwindcss/vite';
import path from 'node:path';
import { defineConfig } from 'vite';
import { VitePWA } from 'vite-plugin-pwa';

// https://vitejs.dev/config/
export default defineConfig({
  build: {
    target: 'esnext',
  },
  plugins: [
    preact({
      reactAliasesEnabled: true,
      include: ['**/*[jt]sx'],
      devToolsEnabled: true,
      babel: {
        plugins: [['babel-plugin-react-compiler', { target: '19' }]],
      },
    }),
    tailwindcss(),
    VitePWA({
      registerType: 'autoUpdate',
      useCredentials: true,
      manifest: {
        theme_color: '#0F172A',
        background_color: '#030711',
        display: 'fullscreen',
        orientation: 'portrait',
        scope: '/',
        start_url: '/',
        name: 'Remote Controller',
        short_name: 'remote-controller',
        description: 'A remote controller for your computer',
        icons: [
          { src: '/icon-192x192.png', sizes: '192x192', type: 'image/png' },
          { src: '/icon-256x256.png', sizes: '256x256', type: 'image/png' },
          { src: '/icon-384x384.png', sizes: '384x384', type: 'image/png' },
          { src: '/icon-512x512.png', sizes: '512x512', type: 'image/png' },
        ],
      },
    }),
  ],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
      'react/compiler-runtime': 'react-compiler-runtime',
    },
  },
});
