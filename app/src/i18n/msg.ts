/**
 * Core-made text in the chosen language. The core sends its English `text`
 * plus a language-neutral `msg` (catalog key and typed arguments); this renders
 * the `msg`, and falls back to the English text for an unknown key or an older
 * backend that sends none.
 */
import { currentLanguage, tx } from "./index";
import { MESSAGES, type MessageKey } from "./messages";

export interface Msg {
  key: string;
  args?: Record<string, MsgArg>;
}

export type MsgArg =
  | { t: "text"; v: string }
  | { t: "count"; v: number }
  | { t: "minutes"; v: number }
  | { t: "msg"; v: Msg }
  | { t: "list"; v: Msg[] };

function known(key: string): key is MessageKey {
  return Object.prototype.hasOwnProperty.call(MESSAGES.en, key);
}

/** Ages as the core writes them: whole minutes, hours or days. */
export function formatMinutes(minutes: number): string {
  if (minutes < 60) return tx("time.min", { n: minutes });
  if (minutes < 24 * 60) return tx("time.hour", { n: Math.floor(minutes / 60) });
  return tx("time.day", { n: Math.floor(minutes / (24 * 60)) });
}

function render(msg: Msg): string | null {
  const count = msg.args?.n;
  const plural =
    count?.t === "count" ? `${msg.key}.${count.v === 1 ? "one" : "other"}` : null;
  const key = plural && known(plural) ? plural : msg.key;
  if (!known(key)) return null;
  const vars: Record<string, string | number> = {};
  for (const [name, arg] of Object.entries(msg.args ?? {})) {
    const value = renderArg(arg);
    if (value === null) return null;
    vars[name] = value;
  }
  return tx(key, vars);
}

function renderArg(arg: MsgArg): string | number | null {
  switch (arg.t) {
    case "text":
      return arg.v;
    case "count":
      return arg.v;
    case "minutes":
      return formatMinutes(arg.v);
    case "msg":
      return render(arg.v);
    case "list": {
      const parts = arg.v.map(render);
      return parts.some((part) => part === null) ? null : parts.join(tx("msg.list_sep"));
    }
  }
}

/** CJK typography: a space stays between Chinese and Latin text, never between two Chinese characters. */
const CJK_GAP = /([\u3400-\u9fff\uff00-\uffef\u3000-\u303f])\s+(?=[\u3400-\u9fff\uff00-\uffef\u3000-\u303f])/g;

/** The message in the active language, else the core's English text. */
export function renderMsg(msg: Msg | null | undefined, fallback: string): string {
  if (!msg) return fallback;
  const text = render(msg);
  if (text === null) return fallback;
  return currentLanguage() === "en" ? text : text.replace(CJK_GAP, "$1");
}

/** A need's question: an agent's own words, or the core's in the chosen language. */
export function needQuestion(need: { question: string; question_msg?: Msg }): string {
  return renderMsg(need.question_msg, need.question);
}

/** Why a need waits, in the chosen language. */
export function needReason(need: { reason: string; reason_msg?: Msg }): string {
  return renderMsg(need.reason_msg, need.reason);
}
