import { createSignal, onCleanup } from "solid-js";
import { listDownloads, saveBook, type DownloadJob } from "./api";

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
