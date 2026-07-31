import { createFileRoute } from "@tanstack/react-router";

export const Route = createFileRoute("/auth/session")({
  server: {
    handlers: {
      GET: async () => Response.json({ ok: true }),
      POST: async () => Response.json({ ok: true }),
    },
  },
});
