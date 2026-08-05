import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

export type FlattenReason = "startup" | "start" | "stop" | "exit" | "manual" | string;

type FlattenEnd = {
  reason: string;
  ok: boolean;
  error?: string | null;
};

/**
 * Overlay for explicit cancel/close operations (stop button).
 * Window close no longer flattens — it only destroys the window so Rust can
 * detach and checkpoint (orders & position stay on the exchange).
 */
export function FlattenOverlay() {
  const { t } = useTranslation();
  const [reason, setReason] = useState<FlattenReason | null>(null);
  const [error, setError] = useState("");
  const exitingRef = useRef(false);

  const visibleReason = reason;

  useEffect(() => {
    let unStart: (() => void) | undefined;
    let unEnd: (() => void) | undefined;
    void (async () => {
      unStart = await listen<{ reason: string }>("flatten-start", (e) => {
        setError("");
        setReason(e.payload.reason || "manual");
      });
      unEnd = await listen<FlattenEnd>("flatten-end", (e) => {
        if (!e.payload.ok && e.payload.error) {
          setError(String(e.payload.error));
          window.setTimeout(() => {
            setReason(null);
            setError("");
          }, 2200);
          return;
        }
        setReason(null);
        setError("");
      });
    })();
    return () => {
      unStart?.();
      unEnd?.();
    };
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void (async () => {
      try {
        const win = getCurrentWindow();
        unlisten = await win.onCloseRequested(async (event) => {
          // Allow close without cancel/flatten. Rust Exit handler checkpoints.
          if (exitingRef.current) {
            event.preventDefault();
            return;
          }
          exitingRef.current = true;
        });
      } catch {
        // Not running inside Tauri window.
      }
    })();
    return () => {
      unlisten?.();
    };
  }, []);

  if (!visibleReason) return null;

  const titleKey =
    visibleReason === "startup"
      ? "app.flattenStartup"
      : visibleReason === "start"
        ? "app.flattenStart"
        : visibleReason === "stop"
          ? "app.flattenStop"
          : visibleReason === "exit"
            ? "app.flattenExit"
            : "app.flattenDefault";

  return (
    <div className="flatten-overlay" role="alertdialog" aria-busy="true" aria-live="assertive">
      <div className="flatten-dialog">
        <div className="flatten-spinner" aria-hidden="true" />
        <h2>{t(titleKey)}</h2>
        <p>{t("app.flattenHint")}</p>
        {error ? <p className="flatten-error">{error}</p> : null}
      </div>
    </div>
  );
}
