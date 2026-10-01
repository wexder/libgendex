export function fileSize(bytes: number): string {
  if (!bytes) return "";
  const units = ["B", "KB", "MB", "GB"];
  let i = 0;
  let n = bytes;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i++;
  }
  return `${n < 10 && i > 0 ? n.toFixed(1) : Math.round(n)} ${units[i]}`;
}

export function titleCase(s: string): string {
  return s.replace(/\b\p{L}/gu, (c) => c.toUpperCase());
}

export function timeAgo(unixSeconds?: number | null): string {
  if (!unixSeconds) return "never";
  const s = Math.max(0, Date.now() / 1000 - unixSeconds);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  return `${Math.floor(s / 86400)} d ago`;
}

/** Deterministic pleasant hue per book so placeholder covers are stable. */
export function hue(seed: string): number {
  let h = 0;
  for (let i = 0; i < seed.length; i++) h = (h * 31 + seed.charCodeAt(i)) >>> 0;
  return h % 360;
}

export function initials(title: string): string {
  const words = title.split(/\s+/).filter((w) => /\p{L}|\d/u.test(w[0] ?? ""));
  return words
    .filter((w) => !/^(the|a|an|of|and|le|la|der|die|das)$/i.test(w))
    .slice(0, 2)
    .map((w) => w[0]!.toUpperCase())
    .join("");
}

export const FORMATS = ["epub", "azw3", "mobi", "fb2", "pdf"];

/// LibGen stores ISO 639-2 codes, mixing the bibliographic and terminology variants.
export const LANGUAGES: { label: string; codes: string }[] = [
  { label: "English", codes: "eng" },
  { label: "German", codes: "ger,deu" },
  { label: "French", codes: "fre,fra" },
  { label: "Spanish", codes: "spa" },
  { label: "Italian", codes: "ita" },
  { label: "Portuguese", codes: "por" },
  { label: "Russian", codes: "rus" },
  { label: "Czech", codes: "cze,ces" },
  { label: "Polish", codes: "pol" },
  { label: "Dutch", codes: "dut,nld" },
  { label: "Chinese", codes: "chi,zho" },
  { label: "Japanese", codes: "jpn" },
  { label: "Hebrew", codes: "heb" },
];

export function languageName(code: string): string {
  const c = code.toLowerCase();
  const known = LANGUAGES.find((l) => l.codes.split(",").includes(c));
  return known ? known.label : c.length === 3 ? c.toUpperCase() : titleCase(c);
}
