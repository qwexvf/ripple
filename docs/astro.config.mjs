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
    remarkPlugins: [remarkAlert, remarkMdLinks, remarkHeadingIds],
    rehypePlugins: [
      rehypeSlug,
      [
        rehypeAutolinkHeadings,
        {
          behavior: 'append',
          properties: { className: ['heading-anchor'], 'aria-label': 'Link to this section' },
          content: { type: 'text', value: '#' },
        },
      ],
    ],
  },
});
