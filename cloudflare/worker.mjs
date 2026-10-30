import { connect } from "cloudflare:sockets";
export { RelayHub } from "./relay.mjs";

// Только фиксированные адреса Telegram, без DNS и произвольных портов.
const DESTINATIONS = new Set([
  "149.154.175.50", "149.154.167.51", "149.154.175.100",
  "149.154.167.91", "149.154.171.5", "91.105.192.100",
  "149.154.175.10", "149.154.167.40", "149.154.175.117",
]);
const MAX_FRAME_BYTES = 256 * 1024;
const MAX_QUEUED_BYTES = 1024 * 1024;

export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    if (url.pathname.startsWith("/relay/")) {
      if (!env.RELAY) return new Response("Relay unavailable", { status: 503 });
      return env.RELAY.get(env.RELAY.idFromName("tg-ws-hub-v1")).fetch(request);
    }
    if (request.method === "GET" && url.pathname === "/health") {
      return Response.json({ service: "tg-ws-bridge", status: "ready" });
    }
    if (request.method !== "GET" || url.pathname !== "/apiws") {
      return new Response("Not found", { status: 404 });
    }
    if ((request.headers.get("Upgrade") || "").toLowerCase() !== "websocket") {
      return new Response("Expected WebSocket", { status: 426 });
    }
    const dst = url.searchParams.get("dst");
    if (!DESTINATIONS.has(dst)) {
      return new Response("Destination is not allowed", { status: 403 });
    }

    const socket = connect({ hostname: dst, port: 443 });
    let timer;
    try {
      await Promise.race([
        socket.opened,
        new Promise((_, reject) => {
          timer = setTimeout(() => reject(new Error("connect timeout")), 5000);
        }),
      ]);
    } catch {
      socket.close();
      return new Response("Upstream unavailable", { status: 502 });
    } finally {
      clearTimeout(timer);
    }

    const [client, server] = Object.values(new WebSocketPair());
    server.binaryType = "arraybuffer";
    server.accept();
    const reader = socket.readable.getReader();
    const writer = socket.writable.getWriter();
    let closed = false;
    let queuedBytes = 0;
    let writes = Promise.resolve();

    function close(code = 1000, reason = "") {
      if (closed) return;
      closed = true;
      try { server.close(code, reason); } catch {}
      try { socket.close(); } catch {}
    }

    server.addEventListener("message", (event) => {
      if (closed) return;
      if (!(event.data instanceof ArrayBuffer)) {
        close(1003, "Binary frames required");
        return;
      }
      const data = new Uint8Array(event.data);
      if (data.byteLength > MAX_FRAME_BYTES || queuedBytes + data.byteLength > MAX_QUEUED_BYTES) {
        close(1009, "Upload queue limit");
        return;
      }
      queuedBytes += data.byteLength;
      writes = writes.then(async () => {
        try {
          if (!closed) await writer.write(data);
        } finally {
          queuedBytes -= data.byteLength;
        }
      }).catch(() => close(1011, "TCP write failed"));
    });
    server.addEventListener("close", () => close());
    server.addEventListener("error", () => close(1011, "WebSocket error"));

    ctx.waitUntil((async () => {
      try {
        while (!closed) {
          const { value, done } = await reader.read();
          if (done) break;
          if (value) server.send(value);
        }
      } catch {
        close(1011, "TCP read failed");
      } finally {
        close();
        await writes;
        reader.releaseLock();
        writer.releaseLock();
      }
    })());

    const protocols = (request.headers.get("Sec-WebSocket-Protocol") || "")
      .split(",").map((value) => value.trim());
    return new Response(null, {
      status: 101,
      webSocket: client,
      headers: protocols.includes("binary") ? { "Sec-WebSocket-Protocol": "binary" } : {},
    });
  },
};
