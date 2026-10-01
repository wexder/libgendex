import { createResource, createSignal, For, Match, onCleanup, onMount, Show, Switch } from "solid-js";
import { health, search, type SearchResponse } from "./api";
import { FORMATS, LANGUAGES } from "./format";
import ResultCard from "./ResultCard";
import StatusBar from "./StatusBar";
import { useDownloads } from "./useDownloads";

type Query = { q: string; ext: string; lang: string };

function readUrl(): Query {
  const p = new URLSearchParams(location.search);
  return { q: p.get("q") ?? "", ext: p.get("ext") ?? "", lang: p.get("lang") ?? "" };
}

function writeUrl(q: Query) {
  const p = new URLSearchParams();
  if (q.q) p.set("q", q.q);
  if (q.ext) p.set("ext", q.ext);
  if (q.lang) p.set("lang", q.lang);
  const url = p.toString() ? `?${p}` : location.pathname;
  history.pushState(null, "", url);
}

async function runSearch(q: Query, ai: boolean): Promise<SearchResponse | undefined> {
  if (!q.q.trim()) return undefined;
  const { data, error } = await search({
    query: { q: q.q, ext: q.ext || undefined, lang: q.lang || undefined, limit: 50, ai },
  });
  if (error) throw new Error(error.error);
  return data;
}

export default function App() {
  const initial = readUrl();
  const [input, setInput] = createSignal(initial.q);
  const [query, setQuery] = createSignal<Query>(initial);
  const [aiEnabled] = createResource(async () => {
    const { data } = await health();
    return !!data && data.ranking !== "none";
  });
  // Instant BM25 + heuristic results first, replaced by the AI-ranked list once it arrives.
  const [results] = createResource(query, (q) => runSearch(q, false));
  const [refined] = createResource(
    () => (aiEnabled() ? query() : undefined),
    (q) => runSearch(q, true),
  );
  const shown = () => (!refined.loading && !refined.error && refined()) || results();
  const downloads = useDownloads();
  let inputEl: HTMLInputElement | undefined;

  const submit = (patch: Partial<Query> = {}) => {
    const next = { ...query(), q: input().trim(), ...patch };
    setQuery(next);
    writeUrl(next);
  };

  onMount(() => {
    const onPop = () => {
      const q = readUrl();
      setInput(q.q);
      setQuery(q);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "/" && document.activeElement !== inputEl) {
        e.preventDefault();
        inputEl?.focus();
      }
    };
    window.addEventListener("popstate", onPop);
    window.addEventListener("keydown", onKey);
    onCleanup(() => {
      window.removeEventListener("popstate", onPop);
      window.removeEventListener("keydown", onKey);
    });
  });

  const searched = () => !!query().q;

  return (
    <div class="app" classList={{ "has-query": searched() }}>
      <header class="top">
        <a
          class="brand"
          href="/"
          onClick={(e) => {
            e.preventDefault();
            setInput("");
            submit({ q: "", ext: "", lang: "" });
          }}
        >
          <span class="logo" aria-hidden="true">b</span>
          libgendex
        </a>
      </header>

      <main>
        <section class="hero">
          <Show when={!searched()}>
            <h1>Find your next read.</h1>
            <p class="sub">Search Library Genesis by title, author, series or ISBN. Results favour clean, e-reader friendly files.</p>
          </Show>
          <form
            class="search"
            role="search"
            onSubmit={(e) => {
              e.preventDefault();
              submit();
            }}
          >
            <svg class="search-icon" viewBox="0 0 24 24" aria-hidden="true">
              <circle cx="11" cy="11" r="7" />
              <path d="m20 20-3.5-3.5" />
            </svg>
            <input
              ref={inputEl}
              type="search"
              placeholder="Title, author, series or ISBN…"
              value={input()}
              onInput={(e) => setInput(e.currentTarget.value)}
              autofocus
              aria-label="Search books"
            />
            <button type="submit" disabled={!input().trim()}>
              Search
            </button>
          </form>
          <div class="filters">
            <div class="chips" role="group" aria-label="Format">
              <button classList={{ chip: true, active: !query().ext }} onClick={() => submit({ ext: "" })}>
                Any format
              </button>
              <For each={FORMATS}>
                {(f) => (
                  <button
                    classList={{ chip: true, active: query().ext === f }}
                    onClick={() => submit({ ext: query().ext === f ? "" : f })}
                  >
                    {f.toUpperCase()}
                  </button>
                )}
              </For>
            </div>
            <select
              class="lang"
              value={query().lang}
              onChange={(e) => submit({ lang: e.currentTarget.value })}
              aria-label="Language"
            >
              <option value="">Any language</option>
              <For each={LANGUAGES}>{(l) => <option value={l.codes}>{l.label}</option>}</For>
            </select>
          </div>
        </section>

        <Show when={searched()}>
          <section class="results" aria-live="polite">
            <Switch>
              <Match when={results.error}>
                <div class="empty error">Search failed: {String(results.error?.message ?? results.error)}</div>
              </Match>
              <Match when={results.loading}>
                <div class="skeletons">
                  <For each={[1, 2, 3, 4]}>{() => <div class="card skeleton" />}</For>
                </div>
              </Match>
              <Match when={shown()?.results.length === 0}>
                <div class="empty">
                  No books found for “{query().q}”. Try fewer words or remove filters.
                </div>
              </Match>
              <Match when={shown()}>
                {(r) => (
                  <>
                    <div class="meta-line">
                      {r().results.length} results · {r().took_ms} ms
                      <Show
                        when={refined.loading}
                        fallback={
                          <Show when={r().results.some((x) => x.scores.ranked_by !== "heuristic")}>
                            <span class="pill ai">AI ranked</span>
                          </Show>
                        }
                      >
                        <span class="pill ai refining">AI refining…</span>
                      </Show>
                    </div>
                    <For each={r().results}>
                      {(res) => (
                        <ResultCard
                          result={res}
                          job={downloads.jobs().find((j) => j.md5 === res.book.md5)}
                          onSave={() => downloads.save(res.book.md5)}
                        />
                      )}
                    </For>
                  </>
                )}
              </Match>
            </Switch>
          </section>
        </Show>
      </main>

      <StatusBar />
    </div>
  );
}
