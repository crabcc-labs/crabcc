# crabcc theme for Forgejo

Applies the [crabcc design system](https://crabcc.app/about) to
git.crabcc.app (Forgejo). Drop-in custom CSS — no patching, no rebuild.

## Quick start

```bash
# 1. Copy the theme into Forgejo's custom directory
cp install/forgejo/crabcc-theme.css /var/lib/forgejo/custom/public/assets/css/

# 2. Tell Forgejo to use it (in app.ini)
echo '[ui]'                        >> /etc/forgejo/app.ini
echo 'THEMES = crabcc,crabcc-dark' >> /etc/forgejo/app.ini
echo 'DEFAULT_THEME = crabcc-dark' >> /etc/forgejo/app.ini

# 3. Restart
systemctl restart forgejo
```

## Docker

Mount the CSS file and set the env vars:

```yaml
# compose.yaml
services:
  forgejo:
    image: codeberg.org/forgejo/forgejo:10
    volumes:
      - ./install/forgejo/crabcc-theme.css:/data/gitea/public/assets/css/theme-crabcc.css:ro
    environment:
      FORGEJO__ui__THEMES: "crabcc,crabcc-dark"
      FORGEJO__ui__DEFAULT_THEME: "crabcc-dark"
```

Or with `docker run`:

```bash
docker run -d \
  -v $(pwd)/install/forgejo/crabcc-theme.css:/data/gitea/public/assets/css/theme-crabcc.css:ro \
  -e FORGEJO__ui__THEMES=crabcc,crabcc-dark \
  -e FORGEJO__ui__DEFAULT_THEME=crabcc-dark \
  codeberg.org/forgejo/forgejo:10
```

## NixOS

```nix
services.forgejo = {
  enable = true;
  settings.ui = {
    THEMES = "crabcc,crabcc-dark";
    DEFAULT_THEME = "crabcc-dark";
  };
};

# Deploy the CSS via systemd tmpfiles or a derivation:
systemd.tmpfiles.rules = [
  "C /var/lib/forgejo/custom/public/assets/css/theme-crabcc.css 0644 forgejo forgejo - ${./crabcc-theme.css}"
];
```

## Two themes included

| Theme | `data-theme` | When to use |
|---|---|---|
| `crabcc-dark` | `[data-theme="crabcc-dark"]` | **Default.** Dark terminal aesthetic: `#0e0e10` page, `#161618` cards, `#ff8c42` accent. Matches the crabcc CLI and dashboard. |
| `crabcc` | `[data-theme="crabcc"]` | Light variant: cream background, terracotta `#d35400` accent. For daylight use. |

Users can switch in Settings → Appearance. The admin sets `DEFAULT_THEME`; individual users can override.

## What gets themed

- **Top nav** — dark card background, JetBrains Mono, active tabs get a 2px hot-orange underline
- **Repo header** — bold mono title, compact spacing
- **Buttons** — 4px radius, accent color for primary actions, muted borders for secondary
- **Cards / segments** — `#161618` background, 6px radius, subtle shadow
- **Code blocks / diffs** — sunken background, JetBrains Mono, green/red washes for added/removed lines
- **File browser** — uppercase column headers, hover wash on rows
- **Forms / inputs** — sunken fields, accent focus ring
- **Labels / badges** — compact mono, pill radius, semantic colors (green=ok, red=danger, amber=warn)
- **Scrollbars** — 8px, themed to match the border color
- **Selection** — terracotta wash

## How to verify

1. Open git.crabcc.app in a browser
2. Check the nav bar — it should be dark (`#161618`) with JetBrains Mono
3. Hover over a button — the border should turn hot orange (`#ff8c42`)
4. View a diff — added lines should be green-wash, removed lines red-wash
5. Open DevTools → Elements → the `<html>` tag should have `data-theme="crabcc-dark"`
6. Check the Settings dropdown — `crabcc-dark` and `crabcc` should appear as theme options

## Updating the theme

The source of truth for design tokens is `https://crabcc.app/_ds/crabcc-design-system-*/tokens/`. When tokens change upstream:

1. Fetch the latest `colors.css`, `fonts.css`, `typography.css`, `spacing.css`, `elevation.css`, `base.css`
2. Update the `:root` token block and both `[data-theme]` blocks in `crabcc-theme.css`
3. Verify the Forgejo-specific selectors still target the right elements (Forgejo upgrades may change class names)

## Logo / favicon

For the logo, place a custom SVG in `custom/public/assets/img/` and reference it in `app.ini`:

```ini
[ui]
LOGO = assets/img/logo.svg
FAVICON = assets/img/favicon.svg
```

The crabcc logo lives at `assets/logo.svg` in this repo (`#ff8c42` on `#0e0e10`).
