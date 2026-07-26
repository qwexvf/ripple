# ripple docs

The documentation site. Astro 6 + React 19 + Tailwind v4, with Pagefind for search.

```bash
bun install
bun dev          # localhost:4321
bun run build    # what CI checks
```

Prose lives in `src/content/docs/`:

- `getting-started.md`, `reference/` — user-facing
- `design/` — the design docs, filenames unchanged from when they lived at `docs/*.md`

A page is a markdown file with `title` and `sidebar.order` frontmatter; the sidebar picks
it up from its directory. Don't write an `# H1` — the layout renders `title` as the
heading. Cross-references stay plain relative `.md` paths so they still work when the file
is read on GitHub; `src/plugins/remark-md-links.mjs` rewrites them to page URLs at build
time, and pins the handful of headings that carry an explicit `{#anchor}`.

`PUBLIC_BASE` sets the deploy base path (e.g. `/ripple/` for a GitHub project page); it
defaults to `/`.

Scaffolded from [qwexvf/astro-docs-template](https://github.com/qwexvf/astro-docs-template) (MIT).
