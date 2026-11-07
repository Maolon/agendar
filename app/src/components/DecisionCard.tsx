import { tx, type MessageKey } from "../i18n";
import { useState, type CSSProperties, type ReactNode } from "react";
import { formatEtaFinish, formatRemaining, OWN_ANSWER_MAX, OWN_OPTION, ownOption } from "../api/format";
import type {
  Criterion,
  DecisionCard as DecisionCardDto,
  Listening,
  Mode,
  ProjectSlug,
  UtcTime,
} from "../api/types";
import { Countdown } from "./Countdown";
import { Facts } from "./Facts";
import { Icon } from "./Icon";
import { NeedSnooze } from "./NeedSnooze";
import { OptionCard } from "./OptionCard";
import { ImageCompare, OptionPicture } from "./OptionImages";
import { ProjectAvatar } from "./ProjectAvatar";
import { ReportLink } from "./Reports";
import { Receipt } from "./Receipt";

export interface PrimaryAction {
  label: string;
  onClick: () => void;
  disabled: boolean;
  kbd: "enter" | "mod-enter" | null;
  hint?: string;
}

export interface ConfirmState {
  label: string;
  onConfirm: () => void;
  onCancel: () => void;
}

const LISTENING_TEXT: Record<Listening, MessageKey> = {
  waiting: "dc.listen.waiting",
  connected: "dc.listen.connected",
  away: "dc.listen.away",
};

interface DecisionCardProps {
  project: ProjectSlug;
  /** The Needs-you item this card answers; shows "Not now" in the main window. */
  needId?: string;
  glyph: string | undefined;
  slice: string;
  card: DecisionCardDto;
  mode: Mode;
  anchor: UtcTime;
  since: UtcTime;
  defaultAt: UtcTime | undefined;
  stalledAgents: number;
  /** Whether an agent hears the answer right away. */
  listening?: Listening;
  selected: string | undefined;
  explicit: boolean;
  onSelect: (optionId: string) => void;
  /** The human's own answer, used when `selected` is the reserved `own`. */
  ownText?: string;
  onOwnText?: (text: string) => void;
  doneWhen: Criterion[];
  onDoneWhen: (criteria: Criterion[] | null) => void;
  receiptTag: string;
  receiptSaved: boolean;
  /** Reachable feature labels from the graph, in reading order. */
  unblockLabels: string[];
  /** Assumption strip for this decision, when the agent proceeded early. */
  assumption?: ReactNode;
  onSeeGraph?: () => void;
  primary: PrimaryAction;
  confirm: ConfirmState | null;
  pending: boolean;
  canMutate: boolean;
  disabledReason?: string;
  error?: { message: string; hint?: string } | null;
}

function bots(count: number) {
  const shown = Math.min(count, 3);
  if (shown === 0) return null;
  return (
    <span className="bots" aria-hidden="true">
      {Array.from({ length: shown }, (_, index) => (
        <span key={index}>
          <Icon name="bot" />
        </span>
      ))}
    </span>
  );
}

/**
 * Decision anatomy: breadcrumb, question, strip, unblocks,
 * grounded ETA trade-off, already-checked facts, option cards, receipt preview
 * and the guarded primary action. The primary label is the chosen option, never
 * "Answer".
 */
export function DecisionCard({
  project,
  needId,
  glyph,
  slice,
  card,
  mode,
  anchor,
  since,
  defaultAt,
  stalledAgents,
  listening,
  selected,
  onSelect,
  ownText = "",
  onOwnText,
  doneWhen,
  onDoneWhen,
  receiptTag,
  receiptSaved,
  unblockLabels,
  assumption,
  onSeeGraph,
  primary,
  confirm,
  pending,
  canMutate,
  disabledReason,
  error,
}: DecisionCardProps) {
  const own = selected === OWN_OPTION;
  const selectedOption = own
    ? ownOption(card, ownText)
    : card.options.find((option) => option.id === selected);
  const recommended = card.recommended.option === selected;
  const columns = Math.max(1, Math.min(card.options.length, 4));
  const pictured = card.options.some((option) => option.image);
  const [viewing, setViewing] = useState<number | null>(null);
  const etaLine =
    selectedOption?.eta && defaultAt && card.on_timeout.kind === "proceed_as_assumption"
      ? tx("dc.eta", {
          done: formatEtaFinish(selectedOption.eta, anchor),
          wait: formatRemaining(defaultAt, anchor),
        })
      : "";

  return (
    <>
      <div className="crumb">
        <ProjectAvatar glyph={glyph} size="sm" />
        <span>{project}</span>
        <span className="sep">›</span>
        <Icon name="flag" className="c3" />
        <span>{slice}</span>
        {needId ? <NeedSnooze id={needId} /> : null}
      </div>
      <h1 className="h">{card.question}</h1>
      {mode === "assume" ? (
        <p className="lede">
          {tx("dc.assumed")}
        </p>
      ) : null}
      <div className="strip">
        <div className="l">
          {bots(stalledAgents)}
          <span>
            {stalledAgents > 0 ? (
              <>
                <b>{stalledAgents}</b> {stalledAgents === 1 ? tx("dc.agents_waiting.one") : tx("dc.agents_waiting.other")}
              </>
            ) : (
              tx("dc.waiting_on_you")
            )}
          </span>
          {listening ? <span className={`listen ${listening}`}>{tx(LISTENING_TEXT[listening])}</span> : null}
        </div>
        <Countdown
          since={since}
          defaultAt={defaultAt}
          now={anchor}
          onTimeout={card.on_timeout}
          chosenLabel={card.options.find((option) => option.id === card.recommended.option)?.label}
        />
      </div>
      {/* The choices come right after the question so they show at any window
          height; the context that explains them follows. */}
      <div
        className="opts"
        style={{ "--opt-cols": String(columns) } as CSSProperties}
        role="group"
        aria-label={tx("dc.options")}
      >
        {card.options.map((option, index) => {
          const optionCard = (
            <OptionCard
              key={option.id}
              option={option}
              index={index}
              selected={option.id === selected}
              recommended={option.id === card.recommended.option}
              anchor={anchor}
              onSelect={onSelect}
            />
          );
          if (!pictured) return optionCard;
          return (
            <div className="opt-col" key={option.id}>
              <OptionPicture
                project={project}
                option={option}
                selected={option.id === selected}
                onOpen={() => setViewing(index)}
              />
              {optionCard}
            </div>
          );
        })}
      </div>
      {onOwnText ? (
        <div className={`own-answer${own ? " sel" : ""}`}>
          <button
            type="button"
            className="own-pick"
            aria-pressed={own}
            onClick={() => onSelect(OWN_OPTION)}
          >
            <Icon name="message-square" />
            {tx("dc.own")}
          </button>
          {own ? (
            <textarea
              autoFocus
              aria-label={tx("dc.own_label")}
              placeholder={tx("dc.own_placeholder")}
              maxLength={OWN_ANSWER_MAX}
              rows={3}
              value={ownText}
              onChange={(event) => onOwnText(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
                  event.preventDefault();
                  if (!primary.disabled) primary.onClick();
                }
              }}
            />
          ) : null}
        </div>
      ) : null}
      {viewing !== null ? (
        <ImageCompare
          project={project}
          options={card.options}
          start={viewing}
          selected={selected}
          recommended={card.recommended.option}
          onSelect={onSelect}
          onClose={() => setViewing(null)}
        />
      ) : null}
      {card.report ? (
        <div className="card-report">
          <span>{tx("dc.report")}</span>
          <ReportLink project={project} blob={card.report} label={tx("dc.open_browser")} />
        </div>
      ) : null}
      {assumption}
      {unblockLabels.length ? (
        <div className="unblocks">
          <Icon name="git-branch" />
          <span>
            {tx("dc.unblocks")} <b>{unblockLabels[0]}</b>
            {unblockLabels.length > 1 ? tx("dc.then", { rest: unblockLabels.slice(1).join(tx("dc.and")) }) : ""}
          </span>
          {onSeeGraph ? (
            <button type="button" className="link" onClick={onSeeGraph}>
              {tx("dc.see_graph")}
            </button>
          ) : null}
        </div>
      ) : null}
      {etaLine ? (
        <div className="eta">
          <Icon name="clock" />
          <span>{etaLine}</span>
        </div>
      ) : null}
      <Facts facts={card.already_checked} />
      <div className="recommendation">
        <Icon name="star" />
        <span>
          {tx("dc.recommended")} <b>{card.options.find((option) => option.id === card.recommended.option)?.label ?? "—"}</b>
          {" · "}
          {tx("dc.confidence", { level: tx(`dc.level.${card.recommended.confidence}`) })} — {card.recommended.because}
        </span>
      </div>
      {selectedOption ? (
        <Receipt
          preview={{
            // Your own words stand where an option's label would.
            chose: own ? { ...selectedOption, label: ownText.trim() || tx("dc.own_label") } : selectedOption,
            ruled_out: card.options.filter((option) => option.id !== selectedOption.id),
            done_when: doneWhen,
            ...(selectedOption.eta ? { expected_eta: selectedOption.eta } : {}),
          }}
          anchor={anchor}
          tag={receiptTag}
          editable={!receiptSaved && canMutate && !pending}
          onDoneWhen={onDoneWhen}
        />
      ) : (
        <p className="muted-note">{tx("dc.pick_first")}</p>
      )}
      {error ? (
        <div className="alert" role="alert">
          <Icon name="alert-triangle" />
          <span className="body">
            <span>{error.message}</span>
            {error.hint ? <span className="hint">{error.hint}</span> : null}
          </span>
        </div>
      ) : null}
      {confirm ? (
        // The warning itself is the amber toast the app shows for this step.
        <div className="acts dc-acts confirming">
          <button type="button" className="btn" onClick={confirm.onCancel} disabled={pending}>
            {tx("action.cancel")}
          </button>
          <button type="button" className="btn primary" onClick={confirm.onConfirm} disabled={pending} data-confirm-action>
            {tx("dc.confirm", { label: confirm.label })}
          </button>
        </div>
      ) : (
        <div className="acts dc-acts">
          {disabledReason ? <span className="hint">{disabledReason}</span> : null}
          {!disabledReason && primary.hint ? <span className="hint">{primary.hint}</span> : null}
          <button
            type="button"
            className="btn primary"
            onClick={primary.onClick}
            disabled={primary.disabled || pending || !canMutate}
            aria-describedby={!canMutate ? "mutation-disabled" : undefined}
            data-primary-action
          >
            {pending ? tx("dc.saving") : primary.label}
            {primary.kbd && !pending ? (
              <kbd>{primary.kbd === "enter" ? "↵" : "⌘↵"}</kbd>
            ) : null}
          </button>
        </div>
      )}
      {!canMutate ? (
        <p className="muted-note" id="mutation-disabled">
          {tx("dl.disabled")}
        </p>
      ) : null}
      <span className="sr-only">
        {recommended
          ? tx("dc.recommended_sr", { level: tx(`dc.level.${card.recommended.confidence}`) })
          : ""}
      </span>
    </>
  );
}