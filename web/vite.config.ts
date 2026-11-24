import { fileURLToPath } from "node:url";

import { foldkit } from "@foldkit/vite-plugin";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";

// What this builds, dist/, is committed and compiled into the clanka binary (crates/cli/build.rs),
// which serves it at `clanka web`. The files keep their names from build to build, with no hash,
// so a rebuild changes them in place; the server sends an ETag in the hash's stead.
export default defineConfig({
  appType: "spa",
  resolve: { alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) } },
  plugins: [tailwindcss(), foldkit()],
  build: {
    rolldownOptions: {
      output: {
        entryFileNames: "assets/app.js",
        chunkFileNames: "assets/[name].js",
        assetFileNames: "assets/[name][extname]",
      },
    },
  },
  // `bun run dev` beside a running `clanka web`: the page comes from Vite, the API from clanka.
  // Open Vite's address with the `#t=…` that `clanka web` printed.
  server: { proxy: { "/api": "http://127.0.0.1:7333" } },
});
