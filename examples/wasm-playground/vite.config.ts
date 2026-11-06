import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

export default defineConfig({
  // Published under /playground/ on the site (https://relatedb.ai/).
  // `vite dev` is unaffected because base only applies to `vite build`.
  base: '/playground/',
  plugins: [react()],
  server: {
    port: 5173,
  },
  build: {
    sourcemap: false,
    // esbuild's minifier renames identifiers into short names that collide
    // with labeled statements used inside dagre/graphlib and
    // cypher-language-support (`t:`, `e:`, ...), producing "Undefined label"
    // at runtime. Terser is label-aware and handles it correctly.
    minify: 'terser',
    terserOptions: {
      compress: { passes: 2 },
      mangle: true,
    },
    chunkSizeWarningLimit: 10000,
    rollupOptions: {
      output: {
        manualChunks(id) {
          if (!id.includes('node_modules')) return undefined;
          // React (+ scheduler / jsx runtime) into a dedicated chunk so every
          // consumer chunk resolves the same CJS-interop namespace. Splitting
          // React across chunks (or leaving it in the entry) breaks
          // `React.memo`/`useLayoutEffect` in `@neo4j-nvl/react` and other CJS
          // vendor packages because their chunk sees a different binding than
          // the one that populated `default.memo` at init time.
          if (id.includes('/react/') || id.includes('/react-dom/') ||
              id.includes('/scheduler/')) {
            return 'vendor-react';
          }
          if (id.includes('/@neo4j-nvl/') || id.includes('/cytoscape') ||
              id.includes('/@neo4j-bloom/dagre/') || id.includes('/graphlib/') ||
              id.includes('/workerpool/') || id.includes('/bin-pack/')) {
            return 'vendor-nvl';
          }
          if (id.includes('/@neo4j-cypher/') || id.includes('/@codemirror/') ||
              id.includes('/@lezer/') || id.includes('/codemirror/')) {
            return 'vendor-cm';
          }
          if (id.includes('/prismjs/') || id.includes('/refractor/')) return 'vendor-prism';
          return undefined;
        },
      },
    },
  },

  css: { devSourcemap: false },
  worker: { format: 'es' },
  optimizeDeps: {
    // These packages are CJS (or have deps that are) and their consumers
    // do `import X from 'x'`. Vite only emits the CJS→ESM default-export
    // shim when the package is pre-bundled by the dep optimizer, hence
    // the explicit include list.
    include: [
      'react',
      'react-dom',
      'react-dom/client',
      'react/jsx-runtime',
      '@neo4j-nvl/base',
      '@neo4j-nvl/react',
      '@neo4j-nvl/interaction-handlers',
      'bin-pack',
      '@neo4j-bloom/dagre',
      'graphlib',
      'cytoscape',
      'cytoscape-cose-bilkent',
      'prismjs',
      'workerpool',
      'lodash.debounce',
    ],
    // Both packages spawn workers via `new URL('./x.mjs', import.meta.url)`.
    // esbuild's dep optimizer does not emit worker files into .vite/deps, so
    // the URL 404s. Serving from real node_modules paths fixes it.
    exclude: ['@neo4j-nvl/layout-workers', '@neo4j-cypher/react-codemirror'],
  },
})
