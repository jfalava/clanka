import { fileURLToPath } from "node:url";

import { foldkit } from "@foldkit/vite-plugin";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";

// The router mounts the console at clanka.jfa.dev/admin, so its files live under /admin/.
export default defineConfig({
  base: "/admin/",
  appType: "spa",
  resolve: { alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) } },
  plugins: [tailwindcss(), foldkit()],
});
