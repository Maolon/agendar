import { tx } from "../i18n";
import { useEffect, useMemo, useState } from "react";
import { cardRisk, guardFor, isRecommendation, optionById, OWN_OPTION, ownOption } from "../api/format";
import { useClient } from "../api/store";
import type { DecisionState, NeedsYouItem, ProjectState } from "../api/types";
import { decisionNodeId, indexGraph, unblockText } from "../graph/traversal";
import { AnswerAction, useAnswerState, type SubmitSource } from "../state/answer";
import { useProjectGraphs } from "../state/graph";
import { Assumptions } from "../components/Assumptions";
import { DecisionCard, type ConfirmState, type PrimaryAction } from "../components/DecisionCard";
import type { UiState } from "../state/ui";

interface DecisionProps {
  project: ProjectState;
  decision: DecisionState;
  need: NeedsYouItem;
  anchor: string;
  canMutate: boolean;
  ui: UiState;
  onSelectOption: (optionId: string) => void;
  onSeeGraph: () => void;
  /** Shared answer action: one authoritative guard for keys, view and receipt. */
  action: AnswerAction;
}

/**
 * The decision view reads the authoritative prepared guard from the shared
 * action state, not only the client-derived card risk. A backend escalation to
 * `choose` or `confirm` therefore governs the buttons, the hints and the
 * keyboard at the same time.
 */
export function Decision({
  project,
  decision,
  need,
  anchor,
  canMutate,
  ui,
  onSelectOption,
  onSeeGraph,
  action,
}: DecisionProps) {
  const answer = useAnswerState(action);
  const client = useClient();
  const card = decision.card;
  const graphs = useProjectGraphs(client, [project.summary.slug], anchor);
  const graph = graphs[project.summary.slug];
  const graphIndex = useMemo(() => (graph ? indexGraph(graph) : null), [graph]);
  const reachable = graphIndex
    ? unblockText(graphIndex, project, decisionNodeId(decision.id))
    : null;
  const unblockLabels =
    reachable?.labels ?? need.unblocks.map((node) => project.features[node]?.title ?? node);
  // The implicit recommended choice follows the card the human sees; the
  // effective guard below decides whether it may commit without a choice.
  const cardGuard = guardFor(cardRisk(card));
  const chosenId =
    ui.option_by_ask[decision.id] ?? (cardGuard === "confirm" ? undefined : card.recommended.option);
  // An answer in the human's own words, kept per question while it is open.
  const [ownTexts, setOwnTexts] = useState<Record<string, string>>({});
  const ownText = ownTexts[decision.id] ?? "";
  const own = chosenId === OWN_OPTION;
  const chosen = own ? ownOption(card, ownText) : optionById(card, chosenId);
  const explicit = ui.explicit_selection_by_ask[decision.id] ?? false;
  const guard = action.guard(decision);
  const criteria = action.criteria(decision);

  const feature = project.features[decision.node];
  const slice = feature?.slice ?? decision.node;

  useEffect(() => {
    action.askChanged(decision.id);
  }, [action, decision.id]);

  useEffect(() => {
    action.revisionChanged(decision.id, decision.revision);
  }, [action, decision.id, decision.revision]);

  function submit(source: SubmitSource): void {
    if (!chosen) return;
    if (own && !ownText.trim()) return;
    void action.submit({
      decision,
      option: chosen,
      ...(own ? { ownAnswer: ownText } : {}),
      explicit,
      source,
      enabled: canMutate,
    });
  }

  const disabledReason = !canMutate
    ? tx("dv.stale")
    : !chosen
      ? tx("dv.pick_first")
      : own && !ownText.trim()
        ? tx("dv.write_first")
      : guard === "confirm" && !explicit
        ? tx("dv.press_confirm")
        : guard === "choose" && !explicit
          ? tx("dv.press_choose")
          : undefined;

  const primary: PrimaryAction = {
    label: own
      ? tx("dv.send_own")
      : chosen && (guard === "easy" || explicit)
        ? chosen.label
        : tx("dv.pick"),
    onClick: () => submit("pointer"),
    disabled:
      !chosen || answer.pending || (guard !== "easy" && !explicit) || (own && !ownText.trim()),
    kbd: guard === "easy" && isRecommendation(card, chosen?.id) ? "enter" : "mod-enter",
    hint:
      guard === "confirm"
        ? tx("dv.hint_confirm")
        : guard === "choose"
          ? tx("dv.hint_choose")
          : undefined,
  };

  const confirmState: ConfirmState | null = answer.confirm
    ? {
        label: answer.confirm.label,
        onConfirm: () => submit("pointer"),
        onCancel: () => action.cancelConfirm(),
      }
    : null;

  const plainEnterEnabled = canMutate && action.canPlainEnter(decision, chosen);

  return (
    <div className="view">
      <DecisionCard
        key={decision.id}
        project={project.summary.slug}
        needId={need.id}
        glyph={project.summary.glyph}
        slice={slice}
        card={card}
        mode={decision.mode}
        anchor={anchor}
        since={need.since}
        defaultAt={decision.default_at}
        stalledAgents={need.stalled_agents}
        {...(project.listening?.[decision.id] ? { listening: project.listening[decision.id] } : {})}
        selected={guard === "easy" || explicit ? chosen?.id : undefined}
        explicit={explicit}
        onSelect={onSelectOption}
        ownText={ownText}
        onOwnText={(text) => {
          action.invalidate(decision.id);
          setOwnTexts((texts) => ({ ...texts, [decision.id]: text }));
        }}
        doneWhen={criteria}
        onDoneWhen={(next) => action.setCriteria(decision.id, next)}
        receiptTag={answer.confirm ? tx("dv.prepared") : tx("dv.preview")}
        receiptSaved={false}
        unblockLabels={unblockLabels}
        assumption={
          <Assumptions
            project={project}
            anchor={anchor}
            canMutate={canMutate}
            action={action}
            only={decision.id}
          />
        }
        onSeeGraph={onSeeGraph}
        primary={primary}
        confirm={confirmState}
        pending={answer.pending}
        canMutate={canMutate}
        disabledReason={disabledReason}
        error={answer.error}
      />
      <span className="sr-only">
        {plainEnterEnabled ? tx("dv.enter") : ""}
      </span>
    </div>
  );
}
