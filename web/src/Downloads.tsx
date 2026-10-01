import { createSignal, For, onCleanup, Show } from "solid-js";
import { listDownloads, saveBook, type DownloadJob } from "./api";
import { fileSize } from "./format";

/** Tracks server-side library downloads, polling only while something is in flight. */
export function useDownloads() {
  const [jobs, setJobs] = createSignal<DownloadJob[]>([]);
  let timer: number | undefined;

  const refresh = async () => {
    const { data } = await listDownloads();
    if (data) setJobs(data);
    const active = data?.some((j) => j.state === "queued" || j.state === "downloading");
    clearTimeout(timer);
    if (active) timer = window.setTimeout(refresh, 1000);
  };

  const save = async (md5: string) => {
    const { data } = await saveBook({ path: { md5 } });
    if (data) setJobs((js) => [data, ...js.filter((j) => j.md5 !== md5)]);
    refresh();
  };

  refresh();
  onCleanup(() => clearTimeout(timer));
  return { jobs, save };
}

export default function Downloads(props: { jobs: DownloadJob[] }) {
  const [open, setOpen] = createSignal(false);
  const active = () => props.jobs.filter((j) => j.state === "queued" || j.state === "downloading").length;

  return (
    <div class="downloads">
      <button class="icon-btn" onClick={() => setOpen(!open())} aria-expanded={open()} title="Library downloads">
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <path d="M4 5h4v14H4zM10 5h4v14h-4zM16.5 5.5l3.8 1-3.4 13-3.8-1z" />
        </svg>
        <Show when={active()}>
          <span class="badge">{active()}</span>
        </Show>
      </button>
      <Show when={open()}>
        <div class="panel" role="dialog" aria-label="Library downloads">
          <h3>Server library</h3>
          <Show when={props.jobs.length} fallback={<p class="muted">Nothing saved in this session yet.</p>}>
            <ul>
              <For each={props.jobs}>
                {(j) => (
                  <li>
                    <div class="job-title">{j.title}</div>
                    <div class="job-meta">
                      <span class={`state ${j.state}`}>{j.state}</span>
                      <Show when={j.state === "downloading" && j.total}>
                        {fileSize(j.bytes)} / {fileSize(j.total)}
                      </Show>
                      <Show when={j.state === "done"}>
                        <code>{j.path}</code>
                      </Show>
                      <Show when={j.error}>
                        <span class="err">{j.error}</span>
                      </Show>
                    </div>
                  </li>
                )}
              </For>
            </ul>
          </Show>
        </div>
      </Show>
    </div>
  );
}
