import { useState } from "react";
import { Menu } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
  SheetTrigger,
} from "@/components/ui/sheet";
import { cn } from "@/lib/utils";
import type { NavGroup } from "@/lib/nav";

export function MobileSidebar({
  groups,
  currentSlug,
  baseHref,
}: {
  groups: NavGroup[];
  currentSlug: string;
  baseHref: string;
}) {
  const [open, setOpen] = useState(false);

  return (
    <Sheet open={open} onOpenChange={setOpen}>
      <SheetTrigger
        render={
          <Button variant="outline" size="icon" aria-label="Open navigation" className="size-8 md:hidden">
            <Menu className="size-4" />
          </Button>
        }
      />
      <SheetContent side="left" className="w-72 max-w-[85vw] overflow-y-auto">
        <SheetHeader>
          <SheetTitle className="font-display text-base font-bold">Docs</SheetTitle>
        </SheetHeader>
        <nav className="px-4 pb-6">
          {groups.map((group) => (
            <div key={group.label} className="mb-6">
              <div className="mb-2 px-2 font-mono text-[0.7rem] font-medium uppercase tracking-[0.14em] text-muted-foreground">
                {group.label}
              </div>
              <ul className="space-y-px">
                {group.items.map((item) => {
                  const isCurrent = item.slug === currentSlug;
                  return (
                    <li key={item.slug}>
                      <a
                        href={baseHref + item.href.replace(/^\//, "")}
                        onClick={() => setOpen(false)}
                        className={cn(
                          "block border-l px-3 py-1.5 text-sm transition-colors",
                          isCurrent
                            ? "border-primary bg-accent font-semibold text-primary"
                            : "border-transparent text-muted-foreground hover:border-border hover:text-foreground",
                        )}
                      >
                        {item.label}
                      </a>
                    </li>
                  );
                })}
              </ul>
            </div>
          ))}
        </nav>
      </SheetContent>
    </Sheet>
  );
}
