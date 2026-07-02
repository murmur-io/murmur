/**
 * FE mirror of the backend's loopback classification (`host_is_loopback`,
 * src-tauri/src/summarize/gateway.rs — `IpAddr::is_loopback` + localhost):
 * `localhost` (case-insensitive), `[::1]`/`::1`, or any IPv4 in 127.0.0.0/8.
 * The 127. range requires a VALID dotted-quad — octets 0-255, no leading
 * zeros — for exact parity with Rust's `IpAddr` parse: `127.999.0.1` and
 * `127.01.0.1` fail that parse on the backend (→ cloud), and a HOSTNAME like
 * `127.evil.com` must never count as local (misclassifying an egress host as
 * loopback would hide the cloud-consent surfaces). Callers keep their own
 * unparseable-URL → cloud fail-safe. Reuse this everywhere the FE decides
 * "is this host loopback" so the classifications can't diverge.
 */
const OCTET = "(?:25[0-5]|2[0-4]\\d|1\\d\\d|[1-9]?\\d)";
const LOOPBACK_V4 = new RegExp(`^127\\.${OCTET}\\.${OCTET}\\.${OCTET}$`);

export function hostIsLoopback(hostname: string): boolean {
  const h = hostname.toLowerCase();
  if (h === "localhost" || h === "[::1]" || h === "::1") return true;
  return LOOPBACK_V4.test(h);
}
