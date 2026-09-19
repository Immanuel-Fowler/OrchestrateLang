# OrchestrateLang website

The project website: an overview page, a docs page, and an examples page. Static HTML,
CSS, and JavaScript with no build step.

## The docs page reads the repository

`docs.html` and `examples.html` do not contain documentation. At page load they fetch
the Markdown files under `docs/` (plus `README.md`, `CHANGELOG.md`, the SDK READMEs, and
the benchmark README) and the `.orch` files under `examples/`, then render them. Editing a
file under `docs/` updates the site; there is nothing to regenerate.

The manifest of pages lives at the top of the script in `docs.html`; add a new
document there when a new file lands in `docs/`. The example list is at the top of
`examples.html`.

## Run it locally

Serve the repository root so the pages can reach `docs/` and `examples/`:

```sh
python3 -m http.server 8000
```

Then open <http://localhost:8000/website/>. Opening the files directly with `file://`
will not work: browsers block `fetch` there.

## Regenerating the Rust snapshots

The examples page shows the `src/main.rs` the compiler writes for each program. The
snapshots are committed so the site needs no Rust toolchain to deploy; refresh them
after a compiler change:

```sh
cargo build --release
python3 website/generate-rust.py
```

Examples whose foreign toolchain is not installed (Zig, Swift, TypeScript) are
recorded as skipped in `generated/manifest.json` and keep their previous snapshot only
if you restore it; install the toolchain to regenerate them.

## Deploy

The site is published at <https://immanuel-fowler.github.io/OrchestrateLang/> from the
`website` branch. `.github/workflows/pages.yml` copies `website/` and the files it
reads into one artifact and publishes it with GitHub Pages on every push to that
branch. To ship a change, merge or push it to `website`:

```sh
git push origin <your-branch>:website
```

The workflow enables Pages on its first run. If that step is refused, enable it once
under **Settings → Pages → Source: GitHub Actions** and re-run the workflow.

## Layout

| File | Purpose |
|---|---|
| `index.html` | Overview: the language in one program, constructs, pipeline, boundary ladder, serverlet family, interop table, latency chart, install, limitations |
| `docs.html` | Renders repository Markdown with `marked`, highlights code with `highlight.js`, draws Mermaid diagrams, and indexes headings for search |
| `examples.html` | Renders every example program with the OrchestrateLang grammar, with a tab for the Rust the compiler generated for it |
| `polyglot.html` | Polyglot support: mechanisms, a decision flow, per-language module files fetched from `examples/modules/`, type rules, toolchains, costs |
| `compare.html` | When to use what: OrchestrateLang beside hand-written Tokio, Erlang/OTP, Akka, and Temporal, plus a gallery that leads the page: five orchestration jobs stepped through one at a time with arrows, numbered pills, arrow keys, swipe, and `#ex-<id>` deep links, each shown beside a language picked from one switcher. Snippets live in `<script type="text/plain" data-ex data-lang>` blocks near the end of the file, so code needs no escaping; the cards, pills, and switcher are generated from them |
| `assets/scorecard-data.js` | The polyglot page's scorecard: 72 languages scored 0 to 5 on eight axes for fitting the C-ABI `load_foreign` slot, with tiers, the route each would need, and reasoning. Edit here to re-score |
| `generate-rust.py` | Snapshots the generated Rust for every example into `generated/` (run after `cargo build --release`) |
| `generated/` | Committed snapshots the examples page shows, plus `manifest.json` with the compiler version and date |
| `assets/site.css` | Design tokens (light and dark), layout, code, tables, pills |
| `assets/site.js` | Theme toggle, repository-root resolution, the `orchestrate` grammar for highlight.js, copy buttons |
| `assets/logo.svg`, `favicon.svg` | The mark: three arcs of unequal length kept in one orbit by the center |

External libraries are loaded from cdnjs with pinned versions: marked 12.0.2,
highlight.js 11.9.0, mermaid 10.9.1. Fonts come from Google Fonts: Bricolage Grotesque,
Instrument Sans, JetBrains Mono.
