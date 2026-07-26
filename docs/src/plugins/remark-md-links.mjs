import path from "node:path";
import { fileURLToPath } from "node:url";

// The docs cross-reference each other as plain relative `.md` paths so they
// stay readable on GitHub. Astro core doesn't rewrite those (Starlight does),
// so on the built site they'd 404. Resolve them to real page URLs here and
// both readers work off the same source.

const CONTENT_ROOT = fileURLToPath(new URL("../content/docs", import.meta.url));
const BASE = (process.env.PUBLIC_BASE ?? "/").replace(/\/+$/, "") + "/";

const MD_LINK = /^([^#?:]+\.md)(#.*)?$/;

export function remarkMdLinks() {
  return (tree, file) => {
    const from = file.history?.[0] ?? file.path;
    if (!from) return;
    const dir = path.dirname(path.resolve(from));

    walk(tree, (node) => {
      if (node.type !== "link") return;
      const m = MD_LINK.exec(node.url);
      if (!m) return;

      const target = path.resolve(dir, m[1]);
      if (!target.startsWith(CONTENT_ROOT)) return;

      const slug = path
        .relative(CONTENT_ROOT, target)
        .replace(/\.md$/, "")
        .replace(/(^|\/)index$/, "");

      node.url = BASE + (slug ? slug + "/" : "") + (m[2] ?? "");
    });
  };
}

// A few headings carry an explicit `{#anchor}` because other docs link to it.
// Neither GitHub nor rehype-slug honours that syntax, so the id is pinned here
// and the marker is dropped from the rendered text.
const HEADING_ID = /\s*\{#([A-Za-z0-9_-]+)\}\s*$/;

export function remarkHeadingIds() {
  return (tree) => {
    walk(tree, (node) => {
      if (node.type !== "heading") return;
      const last = node.children?.[node.children.length - 1];
      if (last?.type !== "text") return;
      const m = HEADING_ID.exec(last.value);
      if (!m) return;

      last.value = last.value.replace(HEADING_ID, "");
      node.data ??= {};
      node.data.hProperties = { ...node.data.hProperties, id: m[1] };
    });
  };
}

function walk(node, fn) {
  fn(node);
  for (const child of node.children ?? []) walk(child, fn);
}
