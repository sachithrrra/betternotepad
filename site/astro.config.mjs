import { defineConfig } from "astro/config";

// Deployed to GitHub Pages as a project site: sachithrrra.github.io/betternotepad.
// `base` must match the repo name so built asset/page links resolve under that path.
export default defineConfig({
  site: "https://sachithrrra.github.io",
  base: "/betternotepad",
});
