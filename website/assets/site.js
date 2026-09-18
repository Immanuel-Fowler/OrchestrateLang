/* OrchestrateLang website — shared behavior.
   No build step. Works when the repository root is served by any static
   file server (docs and examples are fetched from the repo at runtime). */

(function () {
  "use strict";

  /* ---------- where is the repository root? ----------
     Served from the repo:   /website/index.html  -> root is /
     Deployed by Pages:      /index.html          -> root is the page's directory
     (the Pages workflow copies docs/, examples/, README.md, CHANGELOG.md next to the site) */
  const ROOT = (function () {
    const p = location.pathname;
    const i = p.indexOf("/website/");
    return i >= 0 ? p.slice(0, i + 1) : p.replace(/[^/]*$/, "");
  })();
  window.ORCH_ROOT = ROOT;
  window.ORCH_REPO = "https://github.com/Immanuel-Fowler/OrchestrateLang";
  window.ORCH_VERSION = "0.8.0";

  /* ---------- theme ---------- */
  const root = document.documentElement;
  function readTheme() {
    try { return localStorage.getItem("orch-theme"); } catch (e) { return null; }
  }
  function applyTheme(t) {
    if (t === "dark" || t === "light") root.setAttribute("data-theme", t);
    else root.removeAttribute("data-theme");
  }
  applyTheme(readTheme());
  window.orchToggleTheme = function () {
    const dark = root.getAttribute("data-theme") === "dark" ||
      (!root.getAttribute("data-theme") && matchMedia("(prefers-color-scheme: dark)").matches);
    const next = dark ? "light" : "dark";
    applyTheme(next);
    try { localStorage.setItem("orch-theme", next); } catch (e) { /* private window */ }
  };

  /* ---------- OrchestrateLang grammar for highlight.js ----------
     Keyword classes mirror editors/vscode/syntaxes/orchestrate.tmLanguage.json. */
  function registerOrchestrate(hljs) {
    if (hljs.getLanguage("orchestrate")) return;
    hljs.registerLanguage("orchestrate", function (h) {
      return {
        name: "OrchestrateLang",
        aliases: ["orch", "orchestratelang"],
        keywords: {
          keyword: "if else while for in break continue return match try catch let fn task process " +
            "orchestrator serverlet struct enum on on_start on_stop on_crash on_tick on_fixed_tick " +
            "module use load load_foreign start trigger parallel automatic via secret sandbox host grant call drop",
          literal: "true false none",
          type: "int float string bool void option result array handle",
          built_in: "print clock_micros to_string to_int to_float parse_int parse_float sleep stop_orch " +
            "length append remove map filter reduce find any all range some ok err update_orchestrator"
        },
        contains: [
          h.C_LINE_COMMENT_MODE,
          h.C_BLOCK_COMMENT_MODE,
          { className: "string", begin: '"', end: '"',
            contains: [{ className: "subst", begin: /\{/, end: /\}/ }] },
          h.C_NUMBER_MODE,
          { className: "title.function_", begin: /\b[a-z_][A-Za-z0-9_]*(?=\s*\()/ },
          { className: "title.class_", begin: /\b[A-Z][A-Za-z0-9_]*\b/ },
          { className: "operator", begin: /\|>|->|=>|\?/ }
        ]
      };
    });
    hljs.registerAliases(["orch_ffi"], { languageName: "orchestrate" });
  }
  window.orchHighlightAll = function (scope) {
    if (!window.hljs) return;
    registerOrchestrate(window.hljs);
    (scope || document).querySelectorAll("pre code").forEach(function (el) {
      if (el.dataset.highlighted || el.classList.contains("nohighlight")) return;
      window.hljs.highlightElement(el);
    });
  };

  /* ---------- copy buttons ---------- */
  document.addEventListener("click", function (ev) {
    const btn = ev.target.closest("[data-copy]");
    if (!btn) return;
    const target = btn.closest(".code")?.querySelector("pre");
    if (!target || !navigator.clipboard) return;
    navigator.clipboard.writeText(target.innerText).then(function () {
      const old = btn.textContent;
      btn.textContent = "Copied";
      setTimeout(function () { btn.textContent = old; }, 1400);
    });
  });

  /* ---------- helpers other pages use ---------- */
  window.orchFetchText = async function (relPath) {
    const res = await fetch(ROOT + relPath, { cache: "no-cache" });
    if (!res.ok) throw new Error(res.status + " " + res.statusText + " for " + relPath);
    return res.text();
  };
  window.orchSlug = function (text) {
    return String(text).toLowerCase().replace(/<[^>]+>/g, "").replace(/[`*_]/g, "")
      .replace(/[^a-z0-9\s-]/g, "").trim().replace(/\s+/g, "-");
  };

  document.addEventListener("DOMContentLoaded", function () {
    document.querySelectorAll("[data-theme-toggle]").forEach(function (b) {
      b.addEventListener("click", window.orchToggleTheme);
    });
    const here = location.pathname.replace(/.*\//, "") || "index.html";
    document.querySelectorAll(".nav a[href]").forEach(function (a) {
      const href = a.getAttribute("href").replace(/[#?].*$/, "");
      if (href === here || (here === "index.html" && href === "./")) a.setAttribute("aria-current", "page");
    });
    window.orchHighlightAll();
  });
})();
