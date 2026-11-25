/**
 * The message for a `transport_stop` that rejected. After a recording the backend adds the takes
 * it could and then rejects with what went wrong (dropped loop passes, takes that could not be
 * finished); the plain "not running" rejection stays as it is.
 */
export function stopNotice(error: unknown, wasRecording: boolean): string {
  const text = String(error).replace(/^Error:\s*/, "");
  return wasRecording ? `Recording stopped, with problems: ${text}` : text;
}
