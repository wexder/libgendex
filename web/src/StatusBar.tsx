import { createSignal, onCleanup, Show } from "solid-js";
import { indexStatus, refreshIndex, type IndexStatus } from "./api";
import { fileSize, timeAgo } from "./format";

export default function StatusBar() {
  const [status, setStatus] = createSignal<IndexStatus>();
  let timer: number | undefined;

  const load = async () => {
    const { data } = await indexStatus();
    if (data) setStatus(data);
    timer = window.setTimeout(load, data?.running ? 3000 : 30000);
  };
  load();
  onCleanup(() => clearTimeout(timer));

  const lastIndexed = () => Math.max(0, ...(status()?.sources.map((s) => s.indexed_at ?? 0) ?? [0]));
  const pct = () => {
    const s = status();
    return s && s.bytes_total ? Math.round((s.bytes_done / s.bytes_total) * 100) : 0;
  };

  return (
    <footer class="status">
      <Show when={status()}>
        {(s) => (
          <>
            <span>
              <strong>{s().total_books.toLocaleString()}</strong> books indexed · updated {timeAgo(lastIndexed())}
            </span>
            <Show when={s().running}>
              <span class="running">
                <span class="dot" /> {s().phase}
                <Show when={s().bytes_total}>
                  {" "}· {fileSize(s().bytes_done)} / {fileSize(s().bytes_total)} ({pct()}%)
                </Show>
                <Show when={s().bootstrap}>
                  {(p) => (
                    <>
                      <Show when={p().rows_done || p().rows_total}>
                        {" "}· {p().rows_done.toLocaleString()}
                        <Show when={p().rows_total}> / {p().rows_total.toLocaleString()}</Show> rows
                      </Show>
                      <Show when={p().bytes_per_second > 0}>
                        {" "}· {fileSize(p().bytes_per_second)}/s
                      </Show>
                      {" "}· {Math.floor(p().elapsed_seconds / 60)}m elapsed
                    </>
                  )}
                </Show>
              </span>
            </Show>
            <Show when={!s().running && s().last_error}>
              <span class="err" title={s().last_error ?? ""}>last refresh failed</span>
            </Show>
            <Show when={!s().running}>
              <button
                class="link"
                onClick={async () => {
                  await refreshIndex();
                  clearTimeout(timer);
                  setTimeout(load, 500);
                }}
              >
                Refresh index
              </button>
            </Show>
          </>
        )}
      </Show>
    </footer>
  );
}
