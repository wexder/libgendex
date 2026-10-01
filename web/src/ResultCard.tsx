import { Show } from "solid-js";
import type { DownloadJob, SearchResult } from "./api";
import { fileSize, hue, initials, languageName } from "./format";

function Meter(props: { label: string; value: number; hint: string }) {
  const pct = () => Math.round(props.value * 100);
  const tone = () => (pct() >= 75 ? "good" : pct() >= 45 ? "ok" : "poor");
  return (
    <div class="meter" title={`${props.hint}: ${pct()}%`}>
      <span class="meter-label">{props.label}</span>
      <span class="meter-track">
        <span class={`meter-fill ${tone()}`} style={{ width: `${pct()}%` }} />
      </span>
    </div>
  );
}

export default function ResultCard(props: {
  result: SearchResult;
  job?: DownloadJob;
  onSave: () => void;
}) {
  const b = () => props.result.book;
  const h = () => hue(b().title + b().author);
  const saved = () => props.result.in_library || props.job?.state === "done";
  const busy = () => props.job?.state === "queued" || props.job?.state === "downloading";
  const progress = () => {
    const j = props.job;
    if (!j || !j.total) return "";
    return ` ${Math.min(100, Math.round((j.bytes / j.total) * 100))}%`;
  };
  const details = () =>
    [b().year, b().publisher].filter(Boolean).join(" · ");

  return (
    <article class="card">
      <div
        class="cover"
        style={{ "--h": h() } as any}
        aria-hidden="true"
      >
        <span>{initials(b().title)}</span>
        <em>{b().extension}</em>
      </div>
      <div class="body">
        <h2 class="title">{b().title}</h2>
        <div class="author">{b().author || "Unknown author"}</div>
        <Show when={details()}>
          <div class="details">{details()}</div>
        </Show>
        <div class="tags">
          <span class={`fmt fmt-${b().extension}`}>{b().extension.toUpperCase()}</span>
          <Show when={b().filesize}>
            <span class="tag">{fileSize(b().filesize)}</span>
          </Show>
          <Show when={b().language}>
            <span class="tag">{languageName(b().language)}</span>
          </Show>
          <Show when={b().pages}>
            <span class="tag">{b().pages} pages</span>
          </Show>
          <Show when={b().series}>
            <span class="tag series">{b().series}</span>
          </Show>
        </div>
        <div class="meters">
          <Meter label="E-reader fit" value={props.result.scores.ereader} hint="Suitability for e-readers" />
          <Meter label="Match" value={props.result.scores.relevance} hint="Relevance to your search" />
        </div>
      </div>
      <div class="actions">
        <a class="btn primary" href={`/api/books/${b().md5}/file`} download="">
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 4v11m0 0-4.5-4.5M12 15l4.5-4.5M5 19h14" /></svg>
          Download
        </a>
        <button
          class="btn"
          classList={{ done: saved(), failed: props.job?.state === "failed" }}
          disabled={saved() || busy()}
          onClick={props.onSave}
          title={props.job?.error ?? (saved() ? "Already in the server library" : "Save to the server library")}
        >
          <Show when={saved()} fallback={busy() ? `Saving…${progress()}` : props.job?.state === "failed" ? "Retry save" : "Save to library"}>
            ✓ In library
          </Show>
        </button>
      </div>
    </article>
  );
}
