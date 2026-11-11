/**
 * Graph access for views: one lazily fetched `get_project` graph per project.
 *
 * The graph is a projection of the same committed state as the snapshot, so it
 * is refetched whenever the snapshot is re-evaluated. While a refetch is in
 * flight, or when the command fails, views keep the projected text they already
 * have rather than inventing nodes.
 */

import { useEffect, useState } from "react";
import type { AgendarClient } from "../api/client";
import type { Graph, ProjectSlug, Snapshot, UtcTime } from "../api/types";
import { isMotionEligible, type MotionPulse } from "../graph/motion";

/** `null` records a failed fetch; `undefined` means "not asked yet". */
export type GraphRecord = Record<ProjectSlug, Graph | null | undefined>;

/**
 * The last graph each project answered with, per client. A page opened again
 * draws its plan in the first render and refetches behind it.
 */
const kept = new WeakMap<AgendarClient, Map<ProjectSlug, Graph>>();

function keptGraphs(client: AgendarClient, slugs: readonly ProjectSlug[]): GraphRecord {
  const graphs: GraphRecord = {};
  for (const slug of slugs) {
    const graph = kept.get(client)?.get(slug);
    if (graph) graphs[slug] = graph;
  }
  return graphs;
}

/**
 * Fetch every requested project graph for the current snapshot. Results are
 * keyed by slug; the previous graph stays visible until the new one lands.
 */
export function useProjectGraphs(
  client: AgendarClient,
  slugs: readonly ProjectSlug[],
  anchor: UtcTime,
): GraphRecord {
  const [graphs, setGraphs] = useState<GraphRecord>(() => keptGraphs(client, slugs));
  const key = [...new Set(slugs.filter(Boolean))].sort().join("|");

  useEffect(() => {
    if (!key) return;
    let cancelled = false;
    for (const slug of key.split("|")) {
      void client.getProject({ project: slug }).then(
        (data) => {
          if (!kept.has(client)) kept.set(client, new Map());
          kept.get(client)!.set(slug, data.graph);
          if (!cancelled) setGraphs((current) => ({ ...current, [slug]: data.graph }));
        },
        () => {
          if (!cancelled) setGraphs((current) => ({ ...current, [slug]: null }));
        },
      );
    }
    return () => {
      cancelled = true;
    };
  }, [client, key, anchor]);

  return graphs;
}

/**
 * Freshness for one pulse: the feature's own axes plus its session's last
 * event. A pulse for waiting, blocked, stopped, finished or stale work may
 * still update age text, but it never moves.
 */
export function pulseEligible(snapshot: Snapshot, pulse: MotionPulse): boolean {
  const project = snapshot.state.projects[pulse.project];
  if (!project) return false;
  const feature = project.features[pulse.node];
  const session = feature?.session ? project.sessions[feature.session] : undefined;
  return isMotionEligible(feature, snapshot.state.evaluated_at, session?.last_event_ts);
}
