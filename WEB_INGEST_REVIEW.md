# 🕷️ Web Ingest Review: `crawl-to-markdown` skill vs Citadel

> **Date:** 2026-11-23 · **Scope:** read-only review of how each tool turns a URL into Markdown
> **Versions:** crawl4ai 0.9.5 · defuddle 0.19.6 · spider 2.54.2 · spider_transformations 2.39.13 · llm_readability 0.0.17 · fast_html2md 0.0.66 · Citadel at `2216364`

---

## ⚡ TL;DR (read this, skip the rest if busy)

- ✅ **Citadel already wins on:** `<base href>` handling, shadow DOM (render_js), scope rules, `aria-hidden` link skipping.
- ❌ **Citadel loses on:** duplicate pages, a silent page limit, no retries, no soft-404 check, cookie banners, relative links in stored Markdown.
- 🎯 **Do first (all size S):**
  1. Store each page once, with or without a trailing slash
  2. Warn when `max_pages` is hit
  3. Don't mark good pages stale when the channel lags
  4. Retry failed requests once
- 🚫 **Don't port:** defuddle/crwl subprocesses (they break the zero-runtime rule).

---

## 🧭 How to read this doc

| Section | What you get | Time |
|---|---|---|
| [🎯 Action list](#-action-list) | Checkboxes, ranked | 1 min |
| [🔍 Gaps](#-gaps-in-citadel) | What's broken + where | 2 min |
| [🛠️ Proposals](#️-proposed-improvements) | How to fix, test first | 5 min |
| [📊 Comparison](#-full-comparison) | Every dimension, with evidence | Reference |
| [🚫 Not worth porting](#-not-worth-porting) | What to leave alone | 1 min |

**Evidence key:** `py:` = `~/.claude/skills/crawl-to-markdown/scripts/crawl_to_markdown.py` · `web.rs:` = `src/ingest/web.rs` · other files are in the crate named.

---

## 🎯 Action list

Ranked by **impact ÷ effort**. Tick as you go.

- [ ] **#1 · S** Store each page once (`/a` = `/a/`)
- [ ] **#2 · S** Warn when `max_pages` is reached
- [ ] **#3 · S** Don't mark pages stale after the channel lags
- [ ] **#4 · S** Retry once on transient failures
- [ ] **#5 · M** Flag 404 pages served with status 200
- [ ] **#6 · S–M** Strip cookie banners and pagers
- [ ] **#7 · M** Make every link in stored Markdown absolute
- [ ] **#8 · S** Drop tracking parameters from page identity

**Next in line:**
- [ ] **M/L** One Chrome per Sync, not one per sitemap crawl
- [ ] **S** Skip `.json`, `.xml`, `.csv`, `.txt`, `.docx` links

---

## 🔍 Gaps in Citadel

| # | 🐛 Gap | 📍 Evidence |
|---|---|---|
| 1 | `/a` and `/a/` are stored as **two pages** | `web.rs:685-686` uses the full URL, not `canonical_dedup_key` (`web.rs:1068-1076`) |
| 2 | Hitting `max_pages` is **silent** | `web.rs:308` switches to `Preserve`; the only warning is `web.rs:336-339` |
| 3 | A lagging channel **drops pages, then marks them stale** | `web.rs:537`, `:548-550` → `MarkStale` at `web.rs:311` → `local.rs:489-490` |
| 4 | **No retries**: one timeout = `error` page | `web.rs:427-438` never calls `with_retry`; spider defaults to 0 (`website.rs:13763`) |
| 5 | **No soft-404 check**; empty pages stored as Ready | `web.rs:690-691`, `:710-727` |
| 6 | **Query strings** make separate pages (`?utm_source=…`) | `web.rs:230`, `:1068-1076` |
| 7 | **Relative links stay relative** in stored Markdown | fast_html2md `rewriter/anchors.rs:39-49` resolves only `/…` links |
| 8 | **Cookie/consent banners** have no filter | readability skip list has no `cookie`/`consent` (`scorer.rs:19-22`); whole-page fallback `web.rs:798-802` |
| 9 | **One crawl (and one Chrome) per sitemap URL** | `web.rs:376-378`; Chrome launch per crawl `web.rs:531-533` |
| 10 | **Data files crawled as pages** | `.json/.xml/.csv/.txt/.docx` not in `web.rs:85-90` |

> 💡 Gap 7 is **shared**: the skill also leaves `setup/config` and `../v1/old` relative unless the page has `<base href>` (tested; `py:131-136`, `:146-147`).

---

## 🛠️ Proposed improvements

> ✅ All 8 fit the rules: **no new runtime**, **work in the lite build**, **don't touch the Private-origin rule** in `src/sources/origin.rs`.

### 1️⃣ Store each page once · `S`

| | |
|---|---|
| 🔧 **Change** | In `build_page_outcomes` (`web.rs:685-686`), key `seen_uris` on `canonical_dedup_key(&canonical_url)`. Prefer the redirect target if there is one. |
| 🎯 **Why** | Gap 1 |
| 🛡️ **Constraints** | Pure URL logic |
| 🧪 **Test first** | Page at `/docs/` links to `/docs/a` and `/docs/a/` → expect **one** Ready outcome |

### 2️⃣ Warn when the page limit is hit · `S`

| | |
|---|---|
| 🔧 **Change** | In `sync_web_source` (`web.rs:306-312`), when `outcomes.len() >= max_pages`, add: *"max_pages N reached; unvisited pages kept, raise max_pages"* |
| 🎯 **Why** | Gap 2 |
| 🛡️ **Constraints** | Text only; the `Preserve` Lesson is unchanged |
| 🧪 **Test first** | In `honors_max_pages_and_include_override` (`web.rs:2306`), assert a warning mentions `max_pages` |

### 3️⃣ Don't mark good pages stale after a lag · `S`

| | |
|---|---|
| 🔧 **Change** | `run_website_crawl` (`web.rs:537-555`) returns the skipped count. If > 0 → `Preserve` + a warning. Optionally raise the channel size above 256. |
| 🎯 **Why** | Gap 3 |
| 🛡️ **Constraints** | Follows the Lesson: preserve pages a Sync didn't see |
| 🧪 **Test first** | Pull the policy choice into a pure fn; `lagged > 0` and `outcomes < max_pages` → expect `Preserve` |

### 4️⃣ Retry once · `S`

| | |
|---|---|
| 🔧 **Change** | Add `.with_retry(1)` in `configure_website` (`web.rs:427-438`) |
| 🎯 **Why** | Gap 4 |
| 🛡️ **Constraints** | Same spider client, same redirect rule (`web.rs:421-425`) |
| 🧪 **Test first** | `/docs/flaky` returns 503 once, then 200 → expect Ready |
| ⚠️ **Check** | Whether spider's retry also covers Chrome fetches; else label it static-only |

### 5️⃣ Flag 404s served with 200 · `M`

| | |
|---|---|
| 🔧 **Change** | After `normalize_markdown` (`web.rs:715`), add `looks_like_not_found(title, word_count)`: title/H1 matches `404\|page not found\|not found` **and** < ~150 words, or the body is empty → `Failed { http_status: Some(200), error_message: "soft 404" }` |
| 🎯 **Why** | Gap 5 |
| 🛡️ **Constraints** | Marks `error`, never deletes (diff hygiene invariant) |
| 🧪 **Test first** | `/docs/gone` = 200 + `<title>Page not found</title><h1>404</h1>` → Failed |
| ⚠️ **Also test** | A short real page titled "Not found errors" stays Ready |

### 6️⃣ Strip cookie banners and pagers · `S–M`

| | |
|---|---|
| 🔧 **Change** | Add selectors to `ignore_tags` in `extract_page_markdown_and_title` (`web.rs:773-780`): `[id*="cookie" i]`, `[class*="cookie" i]`, `[class*="consent" i]`, `[role="dialog"]`, `.pagination`, `[aria-label*="breadcrumb" i]` |
| 🎯 **Why** | Gap 8 |
| 🛡️ **Constraints** | Uses lol_html, already a dependency (`content.rs:225-235`) |
| 🧪 **Test first** | Static variant of `rendered_markdown` (`web.rs:2021`): no "We use cookies", no "Next" |
| ⚠️ **Also test** | An article that mentions cookies in its body keeps that text |

### 7️⃣ Absolute links in stored Markdown · `M`

| | |
|---|---|
| 🔧 **Change** | After `transform_content`, resolve every `](target)` against `<base href>` (resolved against the final URL) or the final URL, with `Url::join`. Skip `#…`, `mailto:`, absolute URLs. `resolve_seed_url` (`web.rs:1042-1057`) is close already. |
| 🎯 **Why** | Gap 7, which would put Citadel ahead of both tools |
| 🛡️ **Constraints** | Text only, no requests |
| 🧪 **Test first** | Page `/docs/a/page`, `<base href="/docs/v2/">`, links `x`, `../y`, `/z` → three absolute URLs that respect `<base>` |

### 8️⃣ Drop tracking parameters · `S`

| | |
|---|---|
| 🔧 **Change** | In `canonicalize_page_url` / `canonical_dedup_key` (`web.rs:1059-1076`), remove `utm_*`, `gclid`, `fbclid`, `ref`, `_ga`; sort the rest. **Keep** `?v=`, `?lang=`, `?page=`. |
| 🎯 **Why** | Gap 6 |
| 🛡️ **Constraints** | Pure URL logic |
| 🧪 **Test first** | `/docs/a?utm_source=x` + `/docs/a` → one page; `/docs/a?v=2` + `/docs/a` → two |

---

## 📊 Full comparison

<details>
<summary><b>🔗 Link discovery</b> (click to open)</summary>

| Dimension | 🐍 Skill (crwl + defuddle) | 🦀 Citadel (spider) | 📍 Evidence |
|---|---|---|---|
| Which HTML | Rendered page after a 3 s wait, links from the **whole page** | Static: raw HTML. render_js: rendered page after Citadel's script; menus are wrapped in `<nav>`, not removed, so their links are still followed | Skill `py:35-40`, `:74`; crawl4ai `async_crawler_strategy.py:1120` · Citadel `web.rs:38-75`, `:448-457`, `:562-566` |
| Shadow DOM | ❌ Not captured (`flatten_shadow_dom` defaults off) | ✅ render_js copies open shadow roots | `async_configs.py:1912`; `async_crawler_strategy.py:1085-1095` · `web.rs:39-49` |
| `<base href>` | Own parser ignores it; crawl4ai's link list honours it | ✅ Honoured, even a relative one; resolves against the redirected URL | `py:28-31`, `:38-40`; `content_scraping_strategy.py:276-280` · spider `page.rs:4357-4372`, `:4424-4433`, `:7070-7077` |
| Hidden anchors | Followed | `aria-hidden="true"` skipped | `py:28-31` · spider `css_selectors.rs:3-4` |

</details>

<details>
<summary><b>🧹 Filtering and de-duplication</b></summary>

| Dimension | 🐍 Skill | 🦀 Citadel | 📍 Evidence |
|---|---|---|---|
| Scope | Same host + path prefix on a `/` boundary | Same scheme + host + port + prefix, or include/exclude globs; checked in spider **and** before storing | `py:49-56` · `web.rs:126-137`, `:181-199`, `:226-258` |
| Fragment / query / slash | All ignored | Fragment dropped; **query kept**; slash ignored for crawling, **not** for storing | `py:43-46` · `web.rs:1068-1076`, `:653`, `:685-686` |
| Skipped files | pdf, images, archives, css, js, json, xml, csv, xlsx, docx, md, txt | Images, fonts, media, archives, binaries, css/js, wasm. `.md` read as Markdown on purpose | `py:17-20` · `web.rs:85-90`, `:262-264`, `:822-836` |
| Subdomains | Exact host | Off; spider rewrites link scheme to the start page's | `py:52` · `web.rs:433-434`; `page.rs:2916-2918` |

</details>

<details>
<summary><b>🚦 Crawl control</b></summary>

| Dimension | 🐍 Skill | 🦀 Citadel | 📍 Evidence |
|---|---|---|---|
| Order | Breadth-first, batches of `workers` | One crawl from the start URL (4 at a time), then **one crawl per unseen sitemap/llms.txt URL**. Order inside spider: not traced | `py:179-185` · `web.rs:341-388`, `:431` |
| Page limit | Prints a `NOTE` | Stops, keeps unvisited pages, **no warning** | `py:227-228` · `web.rs:306-312`, `:429` |
| 404 served as 200 | ❌ Status only; empty article = fail | ❌ Status only. Reverse case handled (browser 4xx re-checked) | `py:102-103`, `:143-145` · `web.rs:690-704`, `:594-644` |
| Retries | 1 retry (crwl error, hiccup, flat code) | ❌ None; render_js stall stops after 5 min | `py:87-105` · `website.rs:13763`; `web.rs:557-583` |

</details>

<details>
<summary><b>📝 Content → Markdown</b></summary>

| Dimension | 🐍 Skill | 🦀 Citadel | 📍 Evidence |
|---|---|---|---|
| Main content | defuddle scoring | llm_readability after stripping nav/footer/script/style; whole-page fallback when thin | `py:129-139` · `web.rs:766-820`; `content.rs:685-713` |
| Menus / footers / pagers | defuddle selector lists | `<nav>`, `<footer>` removed; class/id `menu`, `header`, `pagination`, `pager` dropped | `constants.js:144` · `web.rs:773-780`; `scorer.rs:19-21`, `:213` |
| Cookie banners | defuddle `consent`, `cookie.law` | ❌ Not found in source | `constants.js:456`, `:459` |
| Hidden content | defuddle drops `[hidden]`, so the script un-hides first (**tested**: both tabs restored) | No hidden filter found; class `hidden` only lowers the score | `py:109-115`; `constants.js:74`; `defuddle.js:926-928` · `scorer.rs:27-31` |
| Title / metadata | defuddle title → H1 override → strip `\| Site`; author, published, description | First H1 → `<title>` → URL path; no description/author | `py:121-126`, `:140-142`, `:152-166` · `normalize.rs:153-168`; `web.rs:710-715`, `:1121-1139` |
| Link rewriting | Root-relative only (+ defuddle when `<base>` present) | Root-relative only | `py:146-147`; `defuddle.js:1281-1319` · `anchors.rs:36-52` |

</details>

---

## 🚫 Not worth porting

| ❌ Skip this | 💬 Why |
|---|---|
| defuddle / crwl subprocesses (Node, Python, Playwright) | Breaks the self-contained binary; Chrome must stay the only external runtime |
| `unhide()` | Only needed because defuddle drops `[hidden]`; Citadel's pipeline doesn't. Confirm with a test first. |
| Dropping **all** query strings | Merges real variants like `?lang=`; use proposal 8 instead |
| The skill's own link parser | Ignores `<base>`; spider's extractor is better |
| Fixed 3 s wait | Citadel's idle-network/DOM wait (10 s cap, `web.rs:453-457`) is better |
| Re-render on flat code blocks | Works around a crwl timing issue Citadel doesn't show |
| Web Clipper frontmatter | Obsidian-specific and date-stamped; a `description` column could be a separate idea |
| Following `aria-hidden` anchors | Usually duplicate icon links |

---

## 🧪 How this was checked

- ✅ `cargo test --lib ingest::web`: **17 / 17 passed**
- ✅ defuddle run on a local test page: with `<base>`, without `<base>`, and after `unhide()`
- ⚠️ The test page was **not** run through Citadel; Citadel's output claims come from source
- ⚠️ No crawl was run against an external site
