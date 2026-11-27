import { useState } from "react";
import { useTranslation } from "@/i18n/hooks";
import { useFiltersStore } from "@/stores/filters";

/**
 * File-size range inputs (MB) for the Workshop browse filter bar.
 *
 * Rendered only where the caller opts in — the Collections view shares
 * `FilterBar` but has no per-item file size, so showing these controls there
 * would be a dead switch.
 *
 * Typing is buffered locally and only committed on Enter or blur. Committing
 * on every keystroke made the backend rescan the whole page budget for "1",
 * "10", "100", "1000" in turn — four full scans to type one number, each one
 * discarding the last. The store therefore only ever holds committed values,
 * and a half-typed number never reaches a request.
 */
export default function SizeRangeInputs() {
  const { t } = useTranslation();
  const min = useFiltersStore((s) => s.filters.size_min_mb);
  const max = useFiltersStore((s) => s.filters.size_max_mb);
  const setFilters = useFiltersStore((s) => s.setFilters);

  // Local drafts. Kept in sync with the store so an external reset (the "clear
  // filters" button) also empties the visible fields.
  const [minDraft, setMinDraft] = useState(min);
  const [maxDraft, setMaxDraft] = useState(max);

  // Sync the drafts when the store changes underneath us (an external reset).
  // Done during render by comparing with the previous value, which is the
  // pattern React recommends over an effect: an effect would render once with
  // stale text, commit it, then render again. It is also what
  // `react-hooks/set-state-in-effect` forbids.
  const [syncedMin, setSyncedMin] = useState(min);
  if (min !== syncedMin) {
    setSyncedMin(min);
    setMinDraft(min);
  }
  const [syncedMax, setSyncedMax] = useState(max);
  if (max !== syncedMax) {
    setSyncedMax(max);
    setMaxDraft(max);
  }

  const commit = (key: "size_min_mb" | "size_max_mb", value: string) => {
    const next = value.trim();
    // Nothing changed: do not reset pagination or trigger a rescan.
    if (next === (key === "size_min_mb" ? min : max)) return;
    setFilters({ [key]: next, page: 1 });
  };

  // Commit directly rather than relying on the blur that follows a focus
  // change: the DOM fires blur asynchronously, so a test (or a fast click
  // elsewhere) can observe the store before it updates.
  const handleKeyDown =
    (key: "size_min_mb" | "size_max_mb", draft: string) =>
    (event: React.KeyboardEvent<HTMLInputElement>) => {
      if (event.key === "Enter") {
        event.preventDefault();
        commit(key, draft);
        event.currentTarget.blur();
      } else if (event.key === "Escape") {
        // Abandon the edit and restore what the store currently holds.
        setMinDraft(min);
        setMaxDraft(max);
        event.currentTarget.blur();
      }
    };

  return (
    <div className="flex items-center gap-1.5" data-testid="size-range-inputs">
      <input
        className="input w-20 text-center"
        inputMode="decimal"
        placeholder={t("filters.size_min_placeholder") || "Min"}
        value={minDraft}
        onChange={(e) => setMinDraft(e.target.value)}
        onBlur={() => commit("size_min_mb", minDraft)}
        onKeyDown={handleKeyDown("size_min_mb", minDraft)}
        aria-label={t("filters.size_min_aria") || "Minimum size in MB"}
        title={t("filters.size_commit_hint") || "Press Enter to apply"}
      />
      <span className="text-xs text-subtle">–</span>
      <input
        className="input w-20 text-center"
        inputMode="decimal"
        placeholder={t("filters.size_max_placeholder") || "Max"}
        value={maxDraft}
        onChange={(e) => setMaxDraft(e.target.value)}
        onBlur={() => commit("size_max_mb", maxDraft)}
        onKeyDown={handleKeyDown("size_max_mb", maxDraft)}
        aria-label={t("filters.size_max_aria") || "Maximum size in MB"}
        title={t("filters.size_commit_hint") || "Press Enter to apply"}
      />
      <span className="text-xs text-subtle">MB</span>
    </div>
  );
}