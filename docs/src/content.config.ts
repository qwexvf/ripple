import { defineCollection, z } from "astro:content";
import { glob } from "astro/loaders";

const docs = defineCollection({
  loader: glob({ pattern: "**/*.{md,mdx}", base: "./src/content/docs" }),
  schema: z.object({
    title: z.string(),
    description: z.string().optional(),
    sidebar: z
      .object({
        label: z.string().optional(),
        order: z.number().optional(),
        hidden: z.boolean().default(false),
      })
      .default({}),
    template: z.enum(["doc", "splash"]).default("doc"),
  }),
});

export const collections = { docs };
