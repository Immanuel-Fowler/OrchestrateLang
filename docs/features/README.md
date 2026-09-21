# Feature design documents

One document per feature: the problem it solves, the design that was chosen, what was
hard about it, and the build steps, each marked with what has shipped. They are working
documents — a shipped feature's doc keeps its plan and records where the built thing
differs from the proposal, because that difference is usually the most useful part.

| Document | Feature | Status |
|---|---|---|
| [secret-serverlets.md](secret-serverlets.md) | A serverlet whose body runs in a separate native executable the orchestrator never contains | Shipped |
| [landline-serverlets.md](landline-serverlets.md) | Serverlet bodies written in another language, over a pipe; library mode, host callbacks, budgets | Python and TypeScript shipped; other runtimes planned |
| [sandboxed-serverlets.md](sandboxed-serverlets.md) | A serverlet whose body runs in a wasmtime guest under a memory cap and a timeout | Shipped |
| [shared-state.md](shared-state.md) | `shared let`: state several concurrent things may touch, behind one mutex | Shipped in 0.13.0 |
| [serverlet-files.md](serverlet-files.md) | Distributable serverlets and the consent boundary that loads them | Design; partly superseded by landlines |

For the principles these were designed against, see
[design-philosophy.md](../design-philosophy.md) and the four foundational documents in
[design/](../design/). For what is planned but not designed, see
[roadmap.md](../roadmap.md).
