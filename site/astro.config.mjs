import { defineConfig } from "astro/config";

// Served from the custom domain at root, not a GitHub Pages project subpath.
export default defineConfig({
  site: "https://betternotepad.sach.cc",
});
