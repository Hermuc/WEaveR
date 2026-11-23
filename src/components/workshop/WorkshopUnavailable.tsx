import { AlertCircle, RefreshCw } from "lucide-react";

import { useTranslation } from "@/i18n/hooks";

interface Props {
  onRetry: () => void;
}

/**
 * Shown when the Workshop page could not be fetched at all (network blocked,
 * steamcommunity.com unreachable). Deliberately separate from the empty state:
 * "no results" and "cannot reach Steam" must never look the same.
 */
export default function WorkshopUnavailable({ onRetry }: Props) {
  const { t } = useTranslation();
  return (
    <div className="flex h-full items-center justify-center p-6">
      <div className="max-w-md rounded-xl border border-white/10 bg-surface-raised/60 p-6 text-center">
        <AlertCircle className="mx-auto size-10 text-danger" />
        <h2 className="mt-3 text-base font-semibold text-foreground">
          {t("messages.workshop_unavailable_title")}
        </h2>
        <p className="mt-2 text-xs leading-relaxed text-muted">
          {t("messages.workshop_unavailable_body")}
        </p>
        <p className="mt-1 text-[11px] text-muted">
          {t("messages.workshop_unavailable_hint")}
        </p>
        <button
          onClick={onRetry}
          className="hover-shimmer mt-4 inline-flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-xs font-semibold text-white transition-opacity hover:opacity-90"
        >
          <RefreshCw className="size-3.5" />
          {t("messages.workshop_retry")}
        </button>
      </div>
    </div>
  );
}
