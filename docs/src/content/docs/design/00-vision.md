---
title: "00 — Vision: one graph across the whole stack"
description: "Why ripple exists: trace a change through frontend, backend, database, and infrastructure from a single query, and turn a feature request into ranked change sites."
sidebar:
  label: "00 — Vision"
  order: 0
---
ripple treats a project as one connected system, not a pile of repositories. Real work spans the whole stack:

```
frontend  ↔  backend  ↔  database  ↔  domain  ↔  spec  ↔  implementation  ↔  infrastructure
```

A change rarely stays in the layer it starts in. The point of ripple is to make that reachable from a single query, regardless of which language or repo the answer lives in.

## Two capabilities it must have

**1. "Is it safe to change this?" — trace the blast radius.**
Every function, type, and variable in every language is a node in one linked graph. Ask about any symbol and ripple traces where it is used — across the frontend, the backend, the database, the infrastructure, and any relation between them — and reports what a change would touch. The query does not care where the chain starts; it follows the edges wherever they lead. This is the `impact` command.

**2. "Implement a feature where X does Y then Z." — rank where to change.**
Because the graph is already linked to everything, a natural-language description of a feature can be resolved to the code that has to change. ripple returns the likely change sites, ranked by risk, and what each one would affect downstream. This is the `locate` command feeding `impact`.

## Why this shape

The stack-spanning view is the whole bet: a structural tool that stops at one language or one repo answers the easy half of the question and leaves the risky half — the cross-service, cross-layer reach — invisible. See [`04-architecture.md`](04-architecture.md) for how the IR boundary and adapter seam keep the graph language-agnostic, and [`15-two-tools-two-jobs.md`](15-two-tools-two-jobs.md) for how impact and review-targeting divide the work.
