// Local-date helpers shared by the UI. The backend uses the same local-calendar
// semantics (capture::timeutil), so "today" means the same day on both sides.

/** "YYYY-MM-DD" in the user's local time zone. */
export function localDateString(d: Date = new Date()): string {
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

/** Shift a "YYYY-MM-DD" string by `days` (negative = earlier). */
export function shiftDate(date: string, days: number): string {
  const d = new Date(date + "T12:00:00");
  d.setDate(d.getDate() + days);
  return localDateString(d);
}
