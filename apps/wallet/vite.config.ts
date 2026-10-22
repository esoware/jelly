import tailwindcss from "@tailwindcss/vite"
import react from "@vitejs/plugin-react"
import { defineConfig } from "vite"

const isDebug = process.env["TAURI_ENV_DEBUG"] === "true"

export default defineConfig({
  plugins: [
    react({
      compiler: true,
      exclude: [/\/node_modules\//u, "**/*.gen.ts"],
    }),
    tailwindcss({ optimize: false }),
  ],
  resolve: { tsconfigPaths: true },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    target: false,
    cssTarget: ["chrome140", "safari18"],
    modulePreload: { polyfill: false },
    assetsInlineLimit: 0,
    minify: isDebug ? false : "oxc",
    sourcemap: isDebug,
    chunkSizeWarningLimit: Infinity,
    rolldownOptions: {
      output: {
        entryFileNames: "assets/[name].js",
        assetFileNames: "assets/[name][extname]",
      },
    },
  },
})
