# Typhon Documentation Site

The full documentation for the Typhon language, built with [Astro](https://astro.build) and [Starlight](https://starlight.astro.build), deployed to GitHub Pages.

## Local development

```bash
cd docs-site
npm install
npm run dev
```

The site is served at `http://localhost:4321/Typhon/` (note the `/Typhon` base).

## Building

```bash
npm run build      # produces dist/
npm run preview    # serves dist/ locally
```

## Deployment

GitHub Actions builds and deploys to GitHub Pages on every push to `main` that touches `docs-site/**` or the workflow file itself. See [.github/workflows/deploy-docs.yml](../.github/workflows/deploy-docs.yml).

The deployed site lives at <https://codehalwell.github.io/Typhon/>.

### One-time GitHub Pages setup

The repository's GitHub Pages settings must be configured to use **GitHub Actions** as the source (not "Deploy from a branch"). In the repo settings: **Settings → Pages → Build and deployment → Source: GitHub Actions**.

## Structure

```
docs-site/
├── astro.config.mjs         # Starlight config, sidebar layout
├── src/
│   ├── assets/              # logos, images imported by MDX
│   ├── content/
│   │   ├── docs/            # the documentation pages (.mdx)
│   │   └── content.config.ts
│   └── styles/
│       └── custom.css       # theme overrides
├── public/                  # static assets served at /Typhon/
│   ├── favicon.svg
│   └── .nojekyll            # bypasses Jekyll on GitHub Pages
└── package.json
```

## Editing content

Pages are MDX (Markdown + JSX components from Starlight). Add a new page by:

1. Creating an `.mdx` file under `src/content/docs/<section>/` with the standard frontmatter:

   ```mdx
   ---
   title: Page Title
   description: One-line description for SEO.
   sidebar:
     order: 3
   ---
   ```

2. Adding the slug to the sidebar in `astro.config.mjs`.

Cross-page links use the format `/Typhon/<section>/<slug>/` (note the leading `/Typhon` and trailing `/`).

## Conventions

- **Front-matter `title` containing a colon** must be quoted: `title: "foo: bar"`.
- **Code fences** prefer the language name; `python` for `.ty` and `.dty` (the syntax is close enough for highlighting).
- **Cross-links** use absolute paths starting with `/Typhon/`.
