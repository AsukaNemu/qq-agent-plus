/** Ambient, theme and ordinary UI animations use their original clocks. */
export const ANIMATION_SPEED = 1;
export const NAVIGATION_SPEED = 3;
/** Opening film uses an independent real-time clock. */
export const OPENING_SPEED = 1.5;

export function openingTime(realSeconds: number) {
  return realSeconds * OPENING_SPEED;
}

export function animationTime(realSeconds: number) {
  return realSeconds * ANIMATION_SPEED;
}

export function animationNow() {
  return animationTime(performance.now() / 1000);
}

export function navigationDuration(duration: number) {
  return duration / NAVIGATION_SPEED;
}

export function openingDuration(duration: number) {
  return duration / OPENING_SPEED;
}

export function navigationDelta(delta: number, cinematic = false) {
  return delta * (cinematic ? 1 : NAVIGATION_SPEED);
}
