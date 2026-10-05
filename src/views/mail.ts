// Mail view — DOM port of MailView / MailField from IslandViewContent.swift:
// send the dropped file by email. Nothing leaves before the Send click.

import { h } from "./dom";
import { Bridge } from "../core/bridge";
import { Sound } from "../core/sound";
import { State } from "../core/state";
import type { ViewActions, ViewHost } from "./views";

/** MailField: a label and a borderless field on a soft rounded strip. */
function field(label: string, input: HTMLInputElement): HTMLElement {
  return h("label", { class: "mail-field" }, h("span", { text: label }), input);
}

export function buildMail(actions: ViewActions): ViewHost {
  const heading = h("div", { class: "mail-head" });
  const to = h("input", { type: "email", placeholder: "address@example.com", spellcheck: "false" }) as HTMLInputElement;
  const subject = h("input", { type: "text", spellcheck: "false" }) as HTMLInputElement;
  const body = h("textarea", { class: "mail-body", spellcheck: "false" }) as HTMLTextAreaElement;
  const status = h("div", { class: "mail-status" });
  const sendBtn = h("button", { class: "btn primary", text: "Send" }) as HTMLButtonElement;
  const cancelBtn = h("button", { class: "btn secondary", text: "Cancel" });

  const el = h(
    "div",
    { class: "view" },
    h(
      "div",
      { class: "card" },
      h(
        "div",
        { class: "stack mail-stack" },
        heading,
        field("To", to),
        field("Subject", subject),
        body,
        status,
        h("div", { class: "actions" }, sendBtn, cancelBtn),
      ),
    ),
  );

  let sending = false;
  let preparedFor: string | null = null;

  async function send() {
    if (sending) return;
    if (!to.value.trim()) {
      status.textContent = "Missing recipient.";
      return;
    }
    sending = true;
    status.textContent = "";
    sendBtn.textContent = "Sending…";
    const recipient = to.value.trim();
    try {
      const how = await Bridge.mailSend(recipient, subject.value, body.value, State.droppedFile?.path ?? null);
      Sound.play("send");
      State.noteMessage =
        how === "resend" ? `Email sent to ${recipient}.` : `Your mail app has the email to ${recipient} ready to send.`;
      State.view = "note";
      State.notify();
      window.setTimeout(() => actions.collapse(), 2000);
    } catch (err) {
      status.textContent = String(err).replace(/^Error:\s*/, "");
      Sound.play("error");
    } finally {
      sending = false;
      sendBtn.textContent = "Send";
    }
  }

  sendBtn.addEventListener("click", () => void send());
  cancelBtn.addEventListener("click", () => actions.setView("choose"));
  for (const input of [to, subject, body]) {
    // Escape closes the island, not the form; Enter in a single-line field sends.
    input.addEventListener("keydown", (e) => {
      const key = (e as KeyboardEvent).key;
      if (key === "Enter" && input !== body) {
        e.preventDefault();
        void send();
      }
      e.stopPropagation();
    });
  }

  return {
    el,
    sync() {
      const name = State.droppedFile?.name ?? null;
      // A new file: a fresh form, the subject defaulting to its name (onAppear).
      if (preparedFor !== (name ?? "")) {
        preparedFor = name ?? "";
        subject.value = name ?? "";
        subject.placeholder = name ?? "Subject";
        body.value = "";
        status.textContent = "";
      }
      heading.replaceChildren(h("b", { text: "New email" }));
      if (name) heading.append(h("span", { text: " with " }), h("span", { class: "mail-file", text: name }));
    },
    focus() {
      to.focus();
    },
  };
}
