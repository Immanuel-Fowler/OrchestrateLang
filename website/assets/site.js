/* OrchestrateLang website — shared behavior.
   No build step. Documentation, examples and the version number are read from the
   public repository's default branch at page load; the files deployed beside the
   site are only a fallback for when that host cannot be reached. */

(function () {
  "use strict";

  /* ---------- where the content comes from ----------
     Documentation, examples and the version number are read from the public
     repository's default branch at page load, so the site follows `main` without
     being redeployed. Nothing about the language is hardcoded here.

     The copies the Pages workflow deploys beside the site are a fallback, used
     only when the raw host cannot be reached (offline, or a blocked network). */
  const SLUG = "Immanuel-Fowler/OrchestrateLang";
  const REF = "main";
  const RAW = "https://raw.githubusercontent.com/" + SLUG + "/" + REF + "/";

  /* the local fallback root: /website/x.html -> /, deployed -> beside the page */
  const ROOT = (function () {
    const p = location.pathname;
    const i = p.indexOf("/website/");
    return i >= 0 ? p.slice(0, i + 1) : "";
  })();

  window.ORCH_ROOT = ROOT;
  window.ORCH_SLUG = SLUG;
  window.ORCH_REF = REF;
  window.ORCH_RAW = RAW;
  window.ORCH_REPO = "https://github.com/" + SLUG;
  window.ORCH_VERSION = null;               // filled in from Cargo.toml on main
  window.orchSourceUrl = function (path) { return RAW + String(path).replace(/^\/+/, ""); };

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
  /* Repository text, live from the default branch, falling back to the deployed copy.
     `cache: "default"` lets the browser honour the raw host's 5 minute cache header
     instead of re-downloading every document on every page view. */
  window.orchFetchText = async function (relPath) {
    const path = String(relPath).replace(/^\/+/, "");
    try {
      const live = await fetch(RAW + path, { cache: "default" });
      if (live.ok) return await live.text();
    } catch (e) { /* unreachable: fall through to the deployed copy */ }

    const local = await fetch(ROOT + path, { cache: "no-cache" });
    if (!local.ok) throw new Error(local.status + " " + local.statusText + " for " + path);
    return local.text();
  };

  /* ---------- version, read from Cargo.toml on the default branch ---------- */
  function parseVersion(toml) {
    const pkg = toml.split(/^\[/m).filter(function (s) { return s.indexOf("package]") === 0; })[0];
    const m = (pkg || toml).match(/^\s*version\s*=\s*"([^"]+)"/m);
    return m ? m[1] : null;
  }

  window.orchVersion = async function () {
    if (window.ORCH_VERSION) return window.ORCH_VERSION;
    let v = null;
    try {
      v = parseVersion(await window.orchFetchText("Cargo.toml"));
    } catch (e) { /* leave the badge as it is rather than show a number we cannot verify */ }
    if (!v) return null;
    window.ORCH_VERSION = v;
    document.querySelectorAll("[data-orch-version]").forEach(function (el) {
      el.textContent = el.dataset.orchVersion === "bare" ? v : "v" + v;
    });
    return v;
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
    window.orchVersion();
  });
})();
