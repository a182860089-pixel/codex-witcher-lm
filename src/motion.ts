import { animate, type AnimationPlaybackControls } from "motion";

const reducedMotion = () =>
  window.matchMedia("(prefers-reduced-motion: reduce)").matches;

let pageControls: AnimationPlaybackControls | undefined;
let dialogControls: AnimationPlaybackControls | undefined;

function originFromTrigger(dialog: HTMLElement, trigger?: HTMLElement | null): string {
  if (!trigger) return "50% 40%";
  const dialogBox = dialog.getBoundingClientRect();
  const triggerBox = trigger.getBoundingClientRect();
  const x = triggerBox.left + triggerBox.width / 2 - dialogBox.left;
  const y = triggerBox.top + triggerBox.height / 2 - dialogBox.top;
  return `${x}px ${y}px`;
}

export async function animatePage(el: HTMLElement): Promise<void> {
  pageControls?.stop();
  el.style.willChange = "transform, opacity";
  if (reducedMotion()) {
    pageControls = animate(el, { opacity: [0, 1] }, { duration: 0.14 });
  } else {
    pageControls = animate(
      el,
      { opacity: [0, 1], y: [6, 0] },
      { type: "spring", bounce: 0, duration: 0.26 },
    );
  }
  await pageControls;
  el.style.willChange = "";
}

export async function animateDialogIn(
  dialog: HTMLElement,
  trigger?: HTMLElement | null,
): Promise<void> {
  dialogControls?.stop();
  dialog.style.transformOrigin = originFromTrigger(dialog, trigger);
  dialog.style.willChange = "transform, opacity";
  if (reducedMotion()) {
    dialogControls = animate(dialog, { opacity: [0, 1] }, { duration: 0.16 });
  } else {
    dialogControls = animate(
      dialog,
      { opacity: [0, 1], scale: [0.98, 1], y: [8, 0] },
      { type: "spring", bounce: 0, duration: 0.28 },
    );
  }
  await dialogControls;
  dialog.style.willChange = "";
}

export async function animateDialogOut(dialog: HTMLElement): Promise<void> {
  dialogControls?.stop();
  dialog.style.willChange = "transform, opacity";
  if (reducedMotion()) {
    dialogControls = animate(dialog, { opacity: 0 }, { duration: 0.12 });
  } else {
    dialogControls = animate(
      dialog,
      { opacity: 0, scale: 0.98, y: 4 },
      { type: "spring", bounce: 0, duration: 0.18 },
    );
  }
  await dialogControls;
  dialog.style.willChange = "";
}

export function springPress(el: HTMLElement): void {
  if (reducedMotion()) return;
  animate(el, { scale: 0.98 }, { type: "spring", bounce: 0, duration: 0.12 });
}

export function springRelease(el: HTMLElement): void {
  if (reducedMotion()) return;
  animate(el, { scale: 1 }, { type: "spring", bounce: 0, duration: 0.2 });
}
