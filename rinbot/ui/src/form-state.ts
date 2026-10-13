// Drafts only live in the current DOM/session. Never persist credentials to storage.
function controlKey(element: HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement): string {
  const scope = element.closest<HTMLElement>("[data-sticker-id],[data-chat-key],.incident-card");
  const windowIndex = scope?.classList.contains("time-window") ? [...scope.parentElement!.children].indexOf(scope) : "";
  return element.id || [windowIndex, scope?.dataset.stickerId ?? scope?.dataset.chatKey ?? scope?.querySelector("strong")?.textContent ?? "", element.name, ...Object.entries(element.dataset).filter(([key]) => key !== "dirty").map(([key, value]) => `${key}=${value}`)].join("|");
}
export function capturePanelState(root: HTMLElement) {
  const controls = [...root.querySelectorAll<HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement>("input,select,textarea")];
  const active = document.activeElement;
  return {
    scroll: root.scrollTop,
    parentScroll: root.parentElement?.scrollTop ?? 0,
    controls: controls.map((element) => ({
      key: controlKey(element),
      dirty: element.dataset.dirty === "true",
      value: element.value,
      checked: element instanceof HTMLInputElement ? element.checked : false,
      focused: element === active,
      start: element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement ? element.selectionStart : null,
      end: element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement ? element.selectionEnd : null,
    })),
    open: [...root.querySelectorAll<HTMLDetailsElement>("details")].map((element) => element.open),
  };
}

export function restorePanelState(root: HTMLElement, state: ReturnType<typeof capturePanelState>) {
  const controls = [...root.querySelectorAll<HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement>("input,select,textarea")];
  for (const entry of state.controls) {
    if (!entry.dirty && !entry.focused) continue;
    const element = controls.find((control) => controlKey(control) === entry.key);
    if (!element || (element instanceof HTMLInputElement && element.type === "file")) continue;
    element.value = entry.value;
    if (element instanceof HTMLInputElement) element.checked = entry.checked;
    element.dataset.dirty = String(entry.dirty);
    if (entry.focused) {
      element.focus({ preventScroll: true });
      if (entry.start !== null && entry.end !== null && (element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement)) {
        try { element.setSelectionRange(entry.start, entry.end); } catch { /* Numeric inputs have no selection range. */ }
      }
    }
  }
  root.querySelectorAll<HTMLDetailsElement>("details").forEach((element, index) => { element.open = state.open[index] ?? false; });
  root.scrollTop = state.scroll;
  if (root.parentElement) root.parentElement.scrollTop = state.parentScroll;
}
