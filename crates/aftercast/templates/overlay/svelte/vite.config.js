import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// Relative URLs: aftercast serves the built page from its own folder.
export default defineConfig({
  base: "./",
  plugins: [svelte()],
  build: { outDir: "dist", emptyOutDir: true, target: "es2022" },
});
