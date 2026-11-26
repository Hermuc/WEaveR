import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "@/i18n/hooks";

import FilterBar from "@/components/workshop/FilterBar";
import WorkshopFilterDrawer from "@/components/workshop/WorkshopFilterDrawer";
import Pagination from "@/components/workshop/Pagination";
import WorkshopCard from "@/components/workshop/WorkshopCard";
import WorkshopGrid from "@/components/workshop/WorkshopGrid";
import WorkshopUnavailable from "@/components/workshop/WorkshopUnavailable";
import DetailsPanel from "@/components/common/DetailsPanel";
import { SkeletonCard } from "@/components/common/Skeleton";
import { useFiltersStore } from "@/stores/filters";
import { useAppStore } from "@/stores/app";
import { pushToast } from "@/stores/toasts";
import { useRefreshStore } from "@/stores/refresh";
import { usePaginationContext } from "@/hooks/usePaginationContext";
import { useWorkshopPage } from "@/hooks/useWorkshopPage";
import { describeDownloadError } from "@/lib/errors";
import { useWatchWhileDownloading } from "@/hooks/useWatchWhileDownloading";
import { inTauri, tryInvokeAction } from "@/lib/tauri";
import { WorkshopItem } from "@/types/workshop";

/**
 * Parse a size bound typed by the user.
 *
 * Returns undefined for anything that is not a finite non-negative number, so
 * a half-typed or nonsense value simply drops that bound instead of sending
 * NaN to the backend.
 */
function parseSizeBound(raw: string): number | undefined {
  if (raw.trim() === "") return undefined;
  const value = Number(raw);
  if (!Number.isFinite(value) || value < 0) return undefined;
  return value;
}

export default function WorkshopView() {
  const { t } = useTranslation();
  const filters = useFiltersStore((s) => s.filters);
  const showAdvanced = useFiltersStore((s) => s.showAdvanced);
  const setPage = useFiltersStore((s) => s.setPage);
  const setViewPage = useFiltersStore((s) => s.setViewPage);
  const getViewPage = useFiltersStore((s) => s.getViewPage);
  const accountIndex = useAppStore((s) => s.accountIndex);
  const [selected, setSelected] = useState<WorkshopItem | null>(null);

  // Get the saved page for workshop view
  const currentPage = getViewPage("workshop");

  // Sync filters.page with the view-specific page
  useEffect(() => {
    if (filters.page !== currentPage) {
      setPage(currentPage);
    }
  }, [currentPage, filters.page, setPage]);

  const scrollContainerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    scrollContainerRef.current?.scrollTo({ top: 0, behavior: "smooth" });
  }, [filters.page]);

  // A size filter cannot be applied to one page in isolation (Steam ignores
  // size parameters and returns a fixed 30 items), so the filtered path runs a
  // different command that walks pages and re-slices them. With no bounds set
  // we keep the original command, which is what makes the unfiltered path
  // behave exactly as before.
  const sizeMinMb = filters.size_min_mb.trim();
  const sizeMaxMb = filters.size_max_mb.trim();
  const sizeFilterActive = sizeMinMb !== "" || sizeMaxMb !== "";

  // Memoised so the object identity is stable between renders: the hook folds
  // `extraArgs` into its cache key and effect dependencies, and a fresh object
  // every render would retrigger the fetch loop.
  const sizeExtraArgs = useMemo(() => {
    if (!sizeFilterActive) return undefined;
    // Tauri 2 maps `min_mb` on the Rust side to `minMb` on the wire, so the
    // invoke payload must use camelCase or every bound arrives as None.
    return {
      minMb: parseSizeBound(sizeMinMb),
      maxMb: parseSizeBound(sizeMaxMb),
      perPage: 30,
    };
  }, [sizeFilterActive, sizeMinMb, sizeMaxMb]);

  const { page, loading, error } = useWorkshopPage({
    cmd: sizeFilterActive ? "workshop_browse_sized" : "workshop_browse",
    filters,
    mockKind: "workshop",
    extraArgs: sizeExtraArgs,
  });

  const items = page?.items ?? [];
  const total = page?.total_items ?? 0;
  const totalPages = page?.total_pages ?? 1;
  // Present only on the size-filtered path; carries "we stopped scanning".
  const sizeMeta = page?.size_filter ?? null;

  // Publish pagination context so global hotkeys (mouse back/forward, etc.)
  // can act on this view's pagination.
  const handlePageChange = (newPage: number) => {
    setPage(newPage);
    setViewPage("workshop", newPage);
  };
  usePaginationContext({
    view: "workshop",
    page: filters.page,
    totalPages,
    onPageChange: handlePageChange,
  });

  const { watch: handleWatch, state: watchState } = useWatchWhileDownloading();

  const handleDownload = async (item: WorkshopItem) => {
    if (item.is_collection) {
      setSelected(item);
      return;
    }
    if (!inTauri) {
      pushToast(t("messages.download_started"), "success");
      return;
    }
    const res = await tryInvokeAction("download_start", {
      pubfileid: item.pubfileid,
      accountIndex,
    });

    if (res.ok) {
      pushToast(t("messages.download_started"), "success");
    } else {
      pushToast(describeDownloadError(res.error, t), "error");
    }
  };

  return (
    <div className="flex h-full flex-col">
      <FilterBar showSizeFilter />
      <div className="relative flex flex-1 overflow-hidden">
        <WorkshopFilterDrawer open={showAdvanced} />
        <div ref={scrollContainerRef} className="flex-1 overflow-auto px-4 pt-3 pb-20">
          {error ? (
            <WorkshopUnavailable onRetry={() => useRefreshStore.getState().trigger()} />
          ) : loading ? (
            <div className="grid grid-cols-[repeat(auto-fill,minmax(190px,1fr))] gap-3">
              {Array.from({ length: 30 }).map((_, i) => (
                <SkeletonCard key={i} />
              ))}
            </div>
          ) : items.length === 0 ? (
            <div className="flex h-64 flex-col items-center justify-center gap-2 text-sm text-muted">
              <span>{t("labels.no_wallpapers_found")}</span>
              {sizeFilterActive && (
                <span className="text-xs text-subtle">
                  {t("labels.size_filter_no_results") ||
                    "当前范围没有找到结果，可放宽体积范围试试。"}
                </span>
              )}
            </div>
          ) : (
            <>
              {sizeMeta?.truncated && (
                <div className="mb-3 rounded-lg bg-amber-500/10 px-3 py-2 text-xs text-amber-600">
                  {t("labels.size_filter_truncated", {
                    discovered: sizeMeta.discovered,
                  }) ||
                    `符合条件的结果较少，已显示全部 ${sizeMeta.discovered} 条，可放宽范围。`}
                </div>
              )}
              <WorkshopGrid
                items={items}
                containerRef={scrollContainerRef}
                renderItem={(item) => (
                  <WorkshopCard
                    key={item.pubfileid}
                    item={item}
                    onOpen={setSelected}
                    onDownload={handleDownload}
                  />
                )}
              />
            </>
          )}
        </div>
      </div>
      <Pagination
        page={filters.page}
        totalPages={totalPages}
        onChange={(newPage) => {
          setPage(newPage);
          setViewPage("workshop", newPage);
        }}
        infoText={
          sizeFilterActive
            ? t("labels.size_filter_discovered", {
                discovered: total,
              }) ||
              `已发现 ${total} 条符合体积条件`
            : total
              ? t("labels.showing_wallpapers", {
                  start: (filters.page - 1) * 30 + 1,
                  end: (filters.page - 1) * 30 + items.length,
                  total,
                })
              : ""
        }
      />
      <DetailsPanel
        kind="workshop"
        item={selected}
        onClose={() => setSelected(null)}
        onDownload={handleDownload}

        onWatch={handleWatch}
        watchState={watchState}
      />
    </div>
  );
}
