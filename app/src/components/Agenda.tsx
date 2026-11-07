import { tx, type MessageKey } from "../i18n";
import { useState } from "react";
import { isApiError } from "../api/client";
import { newRequestId } from "../api/format";
import { useClient } from "../api/store";
import type { FeatureState, ProjectState } from "../api/types";
import {
  deadlineInputValue,
  expectedFields,
  featureFields,
  featurePatch,
  fromDeadlineInput,
  FEATURE_TITLE_MAX,
  GOAL_MAX,
  projectFields,
  projectPatch,
  validateFeatureTitle,
  validateGoal,
  validatePriority,
  type FeatureEdit,
} from "../state/agenda";
import { Icon } from "./Icon";

interface Failure {
  message: string;
  hint?: string;
}

interface GoalEditorProps {
  project: ProjectState;
  canMutate: boolean;
  editedEvent?: string;
  onEdited?: (eventId: string) => void;
  onRefresh?: () => void;
}

/**
 * Inline project-goal edit through `update_project`. The expected field map
 * carries the revision of exactly the patched field, and a successful save is
 * marked "Edited by you" (the projection does not publish who wrote a field).
 */
export function GoalEditor({
  project,
  canMutate,
  editedEvent,
  onEdited,
  onRefresh,
}: GoalEditorProps) {
  const client = useClient();
  const [open, setOpen] = useState(false);
  const [text, setText] = useState(project.summary.goal);
  const [pending, setPending] = useState(false);
  const [failure, setFailure] = useState<Failure | null>(null);

  function begin(): void {
    setText(project.summary.goal);
    setFailure(null);
    setOpen(true);
  }

  async function save(): Promise<void> {
    if (pending) return;
    const error = validateGoal(text);
    if (error) {
      setFailure({ message: error });
      return;
    }
    const edit = { goal: text };
    setPending(true);
    setFailure(null);
    try {
      const data = await client.updateProject({
        request_id: newRequestId(),
        project: project.summary.slug,
        expected_fields: expectedFields(project.field_revisions, projectFields(edit)),
        patch: projectPatch(edit),
      });
      onEdited?.(data.event_id);
      setOpen(false);
      onRefresh?.();
    } catch (cause) {
      setFailure({
        message: cause instanceof Error ? cause.message : tx("ag.goal_failed"),
        ...(isApiError(cause) ? { hint: cause.hint } : {}),
      });
    } finally {
      setPending(false);
    }
  }

  if (open) {
    return (
      <div className="goal goal-edit">
        <Icon name="target" />
        <input
          aria-label={tx("ag.goal_label")}
          type="text"
          value={text}
          maxLength={GOAL_MAX}
          disabled={pending || !canMutate}
          onChange={(event) => {
            setText(event.target.value);
            setFailure(null);
          }}
        />
        <button type="button" className="btn" onClick={() => void save()} disabled={pending || !canMutate}>
          {pending ? tx("tray.saving") : tx("ag.save_goal")}
        </button>
        <button type="button" className="link" onClick={() => setOpen(false)} disabled={pending}>
          {tx("action.cancel")}
        </button>
        {failure ? (
          <span className="error" role="alert">
            {failure.message}
            {failure.hint ? ` — ${failure.hint}` : ""}
          </span>
        ) : null}
      </div>
    );
  }

  return (
    <div className="goal">
      <Icon name="target" />
      {/* The pencil follows the goal's last word, so it never lands mid-sentence. */}
      <span>
        {project.summary.goal || tx("ag.no_goal")}
        <button
          type="button"
          className="goal-edit"
          aria-label={tx("ag.edit_goal")}
          title={tx("ag.edit_goal")}
          onClick={begin}
          disabled={!canMutate}
        >
          <Icon name="pencil" />
        </button>
      </span>
      {editedEvent ? (
        <span className="edited" title={tx("ag.saved_as", { id: editedEvent })}>
          {tx("ag.edited")}
        </span>
      ) : null}
    </div>
  );
}

const FIELD_NAMES: Record<string, MessageKey> = {
  title: "ag.field.title",
  deadline: "ag.field.deadline",
  priority: "ag.field.priority",
};

function fieldName(field: string): string {
  const key = FIELD_NAMES[field];
  return key ? tx(key) : field;
}

interface FeatureEditorProps {
  project: ProjectState;
  feature: FeatureState;
  canMutate: boolean;
  edited?: { fields: string[]; event: string };
  onEdited?: (fields: string[], eventId: string) => void;
  onRefresh?: () => void;
}

/**
 * Inline agenda edit for one plan step: title, deadline (tagged set/clear) and
 * priority 0..3. Only changed fields are patched and guarded.
 */
/**
 * Archive or restore a project. Archiving hides it from the lists, never its
 * history; a human's choice outranks an agent's.
 */
export function ArchiveToggle({
  project,
  canMutate,
  onRefresh,
}: {
  project: ProjectState;
  canMutate: boolean;
  onRefresh?: () => void;
}) {
  const client = useClient();
  const [pending, setPending] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const archived = project.summary.archived;
  async function toggle(): Promise<void> {
    if (pending) return;
    const edit = { archived: !archived };
    setPending(true);
    setFailure(null);
    try {
      await client.updateProject({
        request_id: newRequestId(),
        project: project.summary.slug,
        expected_fields: expectedFields(project.field_revisions, projectFields(edit)),
        patch: projectPatch(edit),
      });
      onRefresh?.();
    } catch (cause) {
      setFailure(cause instanceof Error ? cause.message : tx("ag.not_saved"));
    } finally {
      setPending(false);
    }
  }
  return (
    <>
      <button
        type="button"
        className="btn sm archive-toggle"
        disabled={!canMutate || pending}
        onClick={() => void toggle()}
      >
        <Icon name={archived ? "archive-restore" : "archive"} />
        {archived ? tx("ag.restore") : tx("ag.archive")}
      </button>
      {failure ? (
        <span className="c-red" role="alert">
          {failure}
        </span>
      ) : null}
    </>
  );
}

export function FeatureEditor({
  project,
  feature,
  canMutate,
  edited,
  onEdited,
  onRefresh,
}: FeatureEditorProps) {
  const client = useClient();
  const [open, setOpen] = useState(false);
  const [title, setTitle] = useState(feature.title);
  const [deadline, setDeadline] = useState(deadlineInputValue(feature.deadline));
  const [priority, setPriority] = useState(String(feature.priority));
  const [pending, setPending] = useState(false);
  const [failure, setFailure] = useState<Failure | null>(null);

  const originalDeadline = deadlineInputValue(feature.deadline);

  function begin(): void {
    setTitle(feature.title);
    setDeadline(originalDeadline);
    setPriority(String(feature.priority));
    setFailure(null);
    setOpen(true);
  }

  async function save(): Promise<void> {
    if (pending) return;
    const edit: FeatureEdit = {};
    const titleError = validateFeatureTitle(title);
    if (titleError) {
      setFailure({ message: titleError });
      return;
    }
    if (title.trim() !== feature.title) edit.title = title.trim();

    if (deadline !== originalDeadline) {
      const parsed = fromDeadlineInput(deadline);
      if (!deadline.trim() && feature.deadline) {
        edit.deadline = { op: "clear" };
      } else if (parsed) {
        edit.deadline = { op: "set", value: parsed };
      } else {
        setFailure({ message: tx("ag.deadline_bad") });
        return;
      }
    }

    const priorityError = validatePriority(priority);
    if (priorityError) {
      setFailure({ message: priorityError });
      return;
    }
    if (Number(priority) !== feature.priority) edit.priority = Number(priority);

    const fields = featureFields(edit);
    if (!fields.length) {
      setOpen(false);
      return;
    }

    setPending(true);
    setFailure(null);
    try {
      const data = await client.updateFeature({
        request_id: newRequestId(),
        project: project.summary.slug,
        node: feature.node,
        expected_fields: expectedFields(feature.field_revisions, fields),
        patch: featurePatch(edit),
      });
      onEdited?.(fields, data.event_id);
      setOpen(false);
      onRefresh?.();
    } catch (cause) {
      setFailure({
        message: cause instanceof Error ? cause.message : tx("ag.step_failed"),
        ...(isApiError(cause) ? { hint: cause.hint } : {}),
      });
    } finally {
      setPending(false);
    }
  }

  if (!open) {
    return (
      <span className="agenda-edit">
        <button
          type="button"
          className="link"
          aria-label={tx("ag.edit_x", { x: feature.title })}
          onClick={begin}
          disabled={!canMutate}
        >
          {tx("ag.edit")}
        </button>
        {edited ? (
          <span className="edited" title={tx("ag.saved_as", { id: edited.event })}>
            {tx("ag.edited_fields", { fields: edited.fields.map((field) => fieldName(field)).join(", ") })}
          </span>
        ) : null}
      </span>
    );
  }

  return (
    <form
      className="agenda-form"
      aria-label={tx("ag.edit_x", { x: feature.title })}
      onSubmit={(event) => {
        event.preventDefault();
        void save();
      }}
    >
      <label>
        <span>{tx("ag.f.title")}</span>
        <input
          type="text"
          value={title}
          maxLength={FEATURE_TITLE_MAX}
          disabled={pending || !canMutate}
          onChange={(event) => {
            setTitle(event.target.value);
            setFailure(null);
          }}
        />
      </label>
      <label>
        <span>{tx("ag.f.deadline")}</span>
        <input
          type="datetime-local"
          value={deadline}
          disabled={pending || !canMutate}
          onChange={(event) => {
            setDeadline(event.target.value);
            setFailure(null);
          }}
        />
      </label>
      <label>
        <span>{tx("ag.f.priority")}</span>
        <input
          type="number"
          step={1}
          value={priority}
          disabled={pending || !canMutate}
          onChange={(event) => {
            setPriority(event.target.value);
            setFailure(null);
          }}
        />
      </label>
      <div className="acts">
        <button type="button" className="link" onClick={() => setOpen(false)} disabled={pending}>
          {tx("action.cancel")}
        </button>
        <button type="submit" className="btn" disabled={pending || !canMutate}>
          {pending ? tx("tray.saving") : tx("ag.save_step")}
        </button>
      </div>
      {failure ? (
        <span className="error" role="alert">
          {failure.message}
          {failure.hint ? ` — ${failure.hint}` : ""}
        </span>
      ) : null}
    </form>
  );
}
