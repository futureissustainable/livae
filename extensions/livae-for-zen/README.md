# Livae for Zen

A pause before X/Twitter, Reddit, YouTube and Instagram: a short wait with a quote, then a 15-minute pass. With Livae installed it reads its sites, waits and quotes from Livae (native messaging); without it, `config.js` and `quotes.json` apply.

X and Reddit start at 20 seconds and add five seconds per pass that day. YouTube and Instagram remain at five seconds. A pass lasts 15 minutes. Tune them in `config.js`. To gate another site, add it to `config.js` and to `host_permissions` in `manifest.json`.

The gate automatically continues when the wait ends. “Not now” or Escape leaves. Reduced-motion settings are respected.

For Zen Browser, install the packaged XPI through **Add-ons → Install Add-on From File**. The extension is unsigned and requires a browser configuration that permits this custom extension. Its ID is `livae-for-zen@livae`.

For Chromium development, load this directory unpacked through the browser's extension developer mode.

To package after an edit, create a ZIP of this directory's contents with `manifest.json` at its root and give it an `.xpi` extension. Increment the manifest version for an update.
