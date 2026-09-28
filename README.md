# Better Notepad

A simple, fast plain-text editor for macOS.

## Structure

- [`app/`](app) — the macOS app (Rust, [gpui-kit](https://github.com/longbridge/gpui-kit))
- [`site/`](site) — project website and Sparkle update feed (Astro, deployed to GitHub Pages)

## Development

```sh
cd app
cargo run
```

```sh
cd site
npm install && npm run dev
```

## Credits

Built with [GPUI Kit](https://gpui.rs/), developed by Longbridge and licensed under Apache-2.0.

GPUI Kit is built on [GPUI](https://www.gpui.rs/), the UI framework from Zed Industries, also licensed under Apache-2.0.
