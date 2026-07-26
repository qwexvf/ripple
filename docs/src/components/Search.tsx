import { useEffect, useState } from "react";
import { Search as SearchIcon, FileText, ArrowRight } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Kbd } from "@/components/ui/kbd";
import { Separator } from "@/components/ui/separator";

type PagefindResult = {
  id: string;
  data: () => Promise<{
    url: string;
    meta: { title?: string };
    excerpt: string;
  }>;
};

declare global {
  interface Window {
    pagefind?: { search: (q: string) => Promise<{ results: PagefindResult[] }> };
  }
}

type Hit = { url: string; title: string; excerpt: string };

export function Search({ baseHref }: { baseHref: string }) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<Hit[]>([]);
  const [unavailable, setUnavailable] = useState(false);

  useEffect(() => {
    if (!open || window.pagefind) return;
    (async () => {
      try {
        const url = new URL("pagefind/pagefind.js", window.location.origin + baseHref).href;
        const mod = await import(/* @vite-ignore */ url);
        await mod.options({ baseUrl: baseHref });
        window.pagefind = mod;
        setUnavailable(false);
      } catch {
        setUnavailable(true);
      }
    })();
  }, [open, baseHref]);

  useEffect(() => {
    if (!open) return;
    if (!query.trim()) {
      setHits([]);
      return;
    }
    let cancelled = false;
    (async () => {
      if (!window.pagefind) return;
      const r = await window.pagefind.search(query);
      const top = await Promise.all(r.results.slice(0, 8).map((res) => res.data()));
      if (cancelled) return;
      setHits(top.map((d) => ({ url: d.url, title: d.meta.title ?? d.url, excerpt: d.excerpt })));
    })();
    return () => {
      cancelled = true;
    };
  }, [query, open]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "k") {
        e.preventDefault();
        setOpen((v) => !v);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <>
      <Button
        variant="outline"
        size="sm"
        onClick={() => setOpen(true)}
        className="hidden h-8 gap-2 px-3 font-mono text-xs text-muted-foreground md:inline-flex"
      >
        <SearchIcon className="size-3.5" />
        <span>Search</span>
        <Kbd className="ml-2">⌘K</Kbd>
      </Button>
      <Button
        variant="outline"
        size="icon"
        onClick={() => setOpen(true)}
        aria-label="Search docs"
        className="size-8 md:hidden"
      >
        <SearchIcon className="size-4" />
      </Button>

      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent
          showCloseButton={false}
          className="!max-w-xl !w-[calc(100%-2rem)] !top-24 !translate-y-0 !p-0 !gap-0 overflow-hidden"
        >
          <DialogTitle className="sr-only">Search docs</DialogTitle>
          <DialogDescription className="sr-only">
            Search across every page of the Docs documentation.
          </DialogDescription>

          <div className="flex items-center gap-2 bg-muted/40 px-3 py-2.5">
            <SearchIcon className="size-4 shrink-0 text-muted-foreground" />
            <Input
              type="text"
              autoFocus
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Search docs..."
              className="h-9 border-0 bg-transparent px-1 font-mono text-sm shadow-none focus-visible:border-0 focus-visible:ring-0 dark:bg-transparent"
            />
            <Kbd className="shrink-0">ESC</Kbd>
          </div>

          <Separator />

          <div className="max-h-[60vh] overflow-y-auto p-2">
            {unavailable && (
              <div className="px-3 py-6 text-center font-mono text-xs text-muted-foreground">
                Search index unavailable in dev. Run{" "}
                <code className="rounded border border-border bg-muted px-1.5 py-0.5 text-primary">
                  bun run build
                </code>{" "}
                to generate it.
              </div>
            )}
            {!unavailable && query && hits.length === 0 && (
              <div className="px-3 py-6 text-center font-mono text-xs text-muted-foreground">
                No matches for {query}
              </div>
            )}
            {!unavailable && !query && (
              <div className="px-3 py-6 text-center font-mono text-xs text-muted-foreground/60">
                Type to search…
              </div>
            )}
            {hits.map((hit) => (
              <a
                key={hit.url}
                href={hit.url}
                className="group flex items-start gap-3 rounded-sm px-3 py-2.5 transition-colors hover:bg-accent"
              >
                <FileText className="mt-0.5 size-4 shrink-0 text-primary" />
                <div className="min-w-0 flex-1">
                  <div className="font-display text-sm font-semibold text-foreground">
                    {hit.title}
                  </div>
                  <div
                    className="mt-0.5 line-clamp-2 text-xs text-muted-foreground"
                    dangerouslySetInnerHTML={{ __html: hit.excerpt }}
                  />
                </div>
                <ArrowRight className="size-3.5 shrink-0 self-center opacity-0 transition-opacity group-hover:opacity-60" />
              </a>
            ))}
          </div>

          <Separator />

          <div className="flex items-center justify-between bg-muted/40 px-3 py-2 font-mono text-[0.65rem] uppercase tracking-wider text-muted-foreground/70">
            <span>Pagefind · static</span>
            <span>↵ to open · ESC to close</span>
          </div>
        </DialogContent>
      </Dialog>
    </>
  );
}
