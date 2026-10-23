import { useEffect, useState } from "react";
import { TriangleAlert } from "lucide-react";
import * as api from "@/lib/api";
import { useAccountsStore } from "@/stores/accounts";
import { useT } from "@/lib/i18n";

export function PreviewLinkBanner() {
  const t = useT();
  const storeValue = useAccountsStore((s) => s.status?.previewLinksClientDirs);
  const [fetchedValue, setFetchedValue] = useState<boolean | null>(null);

  useEffect(() => {
    if (storeValue === undefined) {
      let canceled = false;
      void api
        .getStatus("cn")
        .then((st) => {
          if (!canceled) {
            setFetchedValue(st.previewLinksClientDirs ?? false);
          }
        })
        .catch(() => {
          if (!canceled) setFetchedValue(false);
        });
      return () => {
        canceled = true;
      };
    }
  }, [storeValue]);

  const enabled = storeValue ?? fetchedValue ?? false;
  if (!enabled) return null;

  return (
    <div
      role="status"
      className="border-b border-warning/30 bg-warning/10 px-4 py-2 text-xs text-warning"
    >
      <div className="flex items-center gap-2">
        <TriangleAlert className="size-4 shrink-0" aria-hidden="true" />
        <span className="font-medium">{t("shell.previewLinkBanner")}</span>
      </div>
    </div>
  );
}
