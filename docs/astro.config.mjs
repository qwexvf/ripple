// @ts-check
import { defineConfig } from 'astro/config';
import react from '@astrojs/react';
import tailwindcss from '@tailwindcss/vite';
import pagefind from 'astro-pagefind';
import mdx from '@astrojs/mdx';

import rehypeSlug from 'rehype-slug';
import rehypeAutolinkHeadings from 'rehype-autolink-headings';
import { remarkAlert } from 'remark-github-blockquote-alert';
import { remarkMdLinks, remarkHeadingIds } from './src/plugins/remark-md-links.mjs';

// Astro 7 renders markdown with Sätteri by default. We stay on the remark/rehype
// pipeline because remarkAlert is third-party and our two plugins are remark —
// porting all three to MDAST/HAST buys nothing here. Deprecated in Astro 8's terms
// only if we keep the old top-level plugin keys; `unified()` is the supported form.
import { unified } from '@astrojs/markdown-remark';

// https://astro.build/config
export default defineConfig({
  site: 'https://qwexvf.github.io/ripple',
  base: process.env.PUBLIC_BASE ?? '/',
  trailingSlash: 'always',
  integrations: [react(), mdx(), pagefind()],
  vite: {
    plugins: [tailwindcss()],
  },
  markdown: {
    shikiConfig: {
      themes: { light: 'github-light', dark: 'github-dark-dimmed' },
      defaultColor: false,
      wrap: true,
    },
    processor: unified({
      remarkPlugins: [remarkAlert, remarkMdLinks, remarkHeadingIds],
      rehypePlugins: [
        rehypeSlug,
        [
          rehypeAutolinkHeadings,
          {
            behavior: 'append',
            properties: { className: ['heading-anchor'], 'aria-label': 'Link to this section' },
            // The visible `#` is a CSS ::after in prose.css — as a text node it
            // ends up in the heading text Astro collects for the ToC.
            content: [],
          },
        ],
      ],
    }),
  },
});
