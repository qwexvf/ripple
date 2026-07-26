import { getCollection, type CollectionEntry } from "astro:content";

export type NavItem = {
  label: string;
  slug: string;
  href: string;
  order: number;
};

export type NavGroup = {
  label: string;
  items: NavItem[];
};

/**
 * Static sidebar layout. Sections drive grouping + ordering; pages
 * inside each section are sorted by `sidebar.order` (frontmatter)
 * with title fallback. Pages NOT listed by directory are auto-included
 * so dropping a markdown file in src/content/docs is enough.
 */
const SECTIONS: { label: string; dir: string | null }[] = [
  { label: "Start here", dir: null }, // root-level pages
  { label: "Guides", dir: "guides" },
  { label: "Reference", dir: "reference" },
  { label: "Design", dir: "design" },
  { label: "Contributing", dir: "contributing" },
];

const SECTION_LABELS: Record<string, string> = {
  "getting-started": "Getting started",
  index: "Introduction",
};

export async function buildNav(): Promise<NavGroup[]> {
  const all = await getCollection("docs");
  const groups: NavGroup[] = [];

  for (const section of SECTIONS) {
    const items: NavItem[] = all
      .filter((entry) => {
        if (entry.data.sidebar.hidden) return false;
        const dir = entryDir(entry);
        if (section.dir === null) return dir === "";
        return dir === section.dir;
      })
      .map((entry) => {
        const slugBase = entry.id; // e.g. "guides/cookbook" or "index"
        const tail = slugBase.split("/").pop() || "index";
        const label =
          entry.data.sidebar.label ??
          SECTION_LABELS[tail] ??
          entry.data.title;
        return {
          label,
          slug: slugBase,
          href: slugToHref(slugBase),
          order: entry.data.sidebar.order ?? 999,
        };
      })
      .sort((a, b) => {
        if (a.order !== b.order) return a.order - b.order;
        return a.label.localeCompare(b.label);
      });

    if (items.length > 0) {
      groups.push({ label: section.label, items });
    }
  }
  return groups;
}

function entryDir(entry: CollectionEntry<"docs">): string {
  const parts = entry.id.split("/");
  if (parts.length === 1) return "";
  return parts.slice(0, -1).join("/");
}

export function slugToHref(slug: string): string {
  if (slug === "index") return "/";
  return `/${slug}/`;
}

export type AdjacentPages = {
  prev: NavItem | null;
  next: NavItem | null;
};

export async function findAdjacent(currentSlug: string): Promise<AdjacentPages> {
  const groups = await buildNav();
  const flat = groups.flatMap((g) => g.items);
  const idx = flat.findIndex((item) => item.slug === currentSlug);
  if (idx === -1) return { prev: null, next: null };
  return {
    prev: idx > 0 ? flat[idx - 1] : null,
    next: idx < flat.length - 1 ? flat[idx + 1] : null,
  };
}
