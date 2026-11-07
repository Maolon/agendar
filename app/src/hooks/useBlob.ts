/**
 * Read one Vault blob through `read_report`.
 *
 * A report is returned as escaped text or as a short-lived media token; the
 * view never gets a filesystem path and never renders HTML. The hook is keyed
 * by digest so a re-projected delivery with the same bytes does not refetch.
 */

import { tx } from "../i18n";
import { useEffect, useState } from "react";
import { useClient } from "../api/store";
import type { BlobRef } from "../api/types";

export interface BlobView {
  phase: "loading" | "ready" | "error";
  mime?: string;
  text?: string;
  token?: string;
  message?: string;
}

const LOADING: BlobView = { phase: "loading" };

function identity(blob: BlobRef | undefined): string {
  return blob ? `${blob.sha256}:${blob.ext}` : "";
}

/**
 * `attempt` re-reads the blob when it changes: an event can sync from another
 * machine before the blob file it references.
 */
export function useBlob(project: string, blob: BlobRef | undefined, attempt = 0): BlobView {
  const client = useClient();
  const key = identity(blob);
  const [view, setView] = useState<BlobView>(LOADING);

  useEffect(() => {
    if (!blob) {
      setView({ phase: "error", message: tx("blob.none") });
      return;
    }
    let cancelled = false;
    setView(LOADING);
    void client
      .readReport({ project, blob })
      .then((data) => {
        if (cancelled) return;
        setView({
          phase: "ready",
          mime: data.mime,
          ...(data.text !== undefined ? { text: data.text } : {}),
          ...(data.media_token !== undefined ? { token: data.media_token } : {}),
        });
      })
      .catch((cause: unknown) => {
        if (cancelled) return;
        setView({
          phase: "error",
          message: cause instanceof Error ? cause.message : tx("rp.failed"),
        });
      });
    return () => {
      cancelled = true;
    };
    // `key` covers the digest and extension: the blob object identity changes on
    // every refreshed snapshot, and the bytes did not.
  }, [client, project, key, attempt]);

  return view;
}
