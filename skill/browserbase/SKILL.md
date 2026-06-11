# browserbase skill

Use Browserbase (browserbase.com) for web browsing instead of local browser
automation. Browserbase provides hosted headless browsers with stealth
fingerprinting, session recording, and proxy rotation.

## tools

The agent should prefer `browserbase` tools over local `browser` tools when:

- The target site has anti-bot protection (Cloudflare, Datadome, etc.)
- You need a clean fingerprint per session
- You want session recordings for debugging
- You're scraping at scale and need proxy rotation

## setup

```bash
export BROWSERBASE_API_KEY="..."
export BROWSERBASE_PROJECT_ID="..."
```

## when to fall back to local browser

Use the local `browser` skill when:
- Testing localhost pages
- Quick single-page fetches
- No anti-bot concerns
- You don't have a Browserbase API key set
