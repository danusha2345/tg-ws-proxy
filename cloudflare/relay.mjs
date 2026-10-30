import { connect } from "cloudflare:sockets";

export const CHUNK_BYTES = 8192;
const MAX_RANGE_BYTES = 14336;
const RANGE_LOOKAHEAD = 4 * MAX_RANGE_BYTES;
const QUEUE_BYTES = 256 * 1024;
const MAX_SESSIONS = 128;
const IDLE_MS = 90_000;
const DESTINATIONS = new Set([
  "149.154.175.50", "149.154.167.51", "149.154.175.100",
  "149.154.167.91", "149.154.171.5", "91.105.192.100",
  "149.154.175.10", "149.154.167.40", "149.154.175.117",
]);

function reply(status, body = null, headers = {}) {
  return new Response(body, { status, headers: {
    "Cache-Control": "no-store, no-transform",
    "Content-Type": "application/octet-stream",
    "X-Tg-Relay": "1",
    ...headers,
  } });
}

async function bodyBytes(request, limit) {
  const parts = [];
  let size = 0;
  if (!request.body) return new Uint8Array();
  const reader = request.body.getReader();
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > limit) {
        await reader.cancel();
        throw new Error("body limit");
      }
      parts.push(value);
    }
  } finally { reader.releaseLock(); }
  const result = new Uint8Array(size);
  let offset = 0;
  for (const part of parts) { result.set(part, offset); offset += part.byteLength; }
  return result;
}

// Один hub разделяет только время работы DO. TCP, очереди и номера пакетов
// остаются независимыми у каждой сессии; содержимое MTProto не расшифровывается.
export class RelayHub {
  constructor(ctx) {
    this.ctx = ctx;
    this.sessions = new Map();
    this.alarmScheduled = false;
  }

  close(sid, reason = "client-close") {
    const session = this.sessions.get(sid);
    if (!session) return;
    this.sessions.delete(sid);
    console.log("relay-close", JSON.stringify({id: sid.slice(0, 8), dst: session.dst, reason,
      read: session.readBytes || 0, delivered: session.downBytes || 0, uploaded: session.upBytes || 0,
      upSeq: session.upSeq, downSeq: session.downSeq, queued: session.queued,
      ageMs: Date.now() - session.created, idleMs: Date.now() - session.activity}));
    session.closed = true;
    session.wakeData?.();
    session.wakeSpace?.();
    for (const wake of session.rangeWaiters || []) wake();
    try { session.socket.close(); } catch {}
  }

  async alarm() {
    const now = Date.now();
    for (const [sid, session] of this.sessions) {
      if (now - session.activity > IDLE_MS) this.close(sid, "idle-expiry");
    }
    if (this.sessions.size) await this.ctx.storage.setAlarm(now + 30_000);
    else this.alarmScheduled = false;
  }

  async pump(session) {
    const reader = session.socket.readable.getReader();
    try {
      while (!session.closed) {
        if (session.queued >= QUEUE_BYTES) {
          await new Promise((resolve) => { session.wakeSpace = resolve; });
          session.wakeSpace = null;
          continue;
        }
        const { value, done } = await reader.read();
        if (done) break;
        session.readBytes += value.byteLength;
        session.queue.push(value);
        session.queued += value.byteLength;
        session.wakeData?.();
        for (const wake of session.rangeWaiters) wake();
      }
    } catch (error) {
      session.readError = String(error).slice(0, 240);
      console.warn("relay-tcp-read-error", session.dst, session.readError);
    } finally {
      session.eof = true;
      session.wakeData?.();
      for (const wake of session.rangeWaiters) wake();
      reader.releaseLock();
    }
  }

  take(session, limit = CHUNK_BYTES) {
    const result = new Uint8Array(Math.min(limit, session.queued));
    let count = 0;
    while (count < result.byteLength) {
      const head = session.queue[0];
      const size = Math.min(head.byteLength, result.byteLength - count);
      result.set(head.subarray(0, size), count);
      if (size === head.byteLength) session.queue.shift();
      else session.queue[0] = head.subarray(size);
      count += size;
    }
    session.queued -= count;
    session.wakeSpace?.();
    return result;
  }

  range(session, offset, limit) {
    let skip = offset - session.rangeAck;
    const length = Math.min(limit, session.queued - skip);
    const result = new Uint8Array(Math.max(0, length));
    let count = 0;
    for (const part of session.queue) {
      if (skip >= part.byteLength) { skip -= part.byteLength; continue; }
      const size = Math.min(part.byteLength - skip, result.byteLength - count);
      result.set(part.subarray(skip, skip + size), count);
      count += size;
      skip = 0;
      if (count === result.byteLength) break;
    }
    return result;
  }

  async fetch(request) {
    const url = new URL(request.url);
    const sid = url.searchParams.get("sid") || "";
    if (!/^[a-f0-9]{32}$/.test(sid)) return reply(400);
    const action = url.pathname.slice("/relay/".length);
    if (action === "open" && request.method === "POST") {
      const dst = url.searchParams.get("dst");
      if (!DESTINATIONS.has(dst)) return reply(403);
      let init;
      try { init = await bodyBytes(request, 64); } catch { return reply(413); }
      if (init.byteLength !== 64) return reply(400);
      const existing = this.sessions.get(sid);
      if (existing) {
        if (existing.dst !== dst || !existing.init.every((b, i) => b === init[i])) return reply(409);
        existing.activity = Date.now();
        return await existing.ready ? reply(200, null, { "X-Relay-Ranges": "1" }) : reply(502);
      }
      if (this.sessions.size >= MAX_SESSIONS) return reply(503);
      const socket = connect({ hostname: dst, port: 443 });
      const session = {
        socket, dst, init, activity: Date.now(), created: Date.now(), queue: [], queued: 0,
        readBytes: 0, downBytes: 0, upBytes: 0,
        rangeAck: 0, rangeWaiters: new Set(), rangeMode: false,
        upSeq: 0, downSeq: 0, flight: null, closed: false, eof: false,
        downBusy: false, upTail: Promise.resolve(), writer: socket.writable.getWriter(),
      };
      this.sessions.set(sid, session);
      session.ready = (async () => {
        let timer;
        try {
          await Promise.race([socket.opened, new Promise((_, reject) => {
            timer = setTimeout(() => reject(new Error("connect timeout")), 5000);
          })]);
          await session.writer.write(init);
          this.pump(session);
          if (!this.alarmScheduled) {
            this.alarmScheduled = true;
            await this.ctx.storage.setAlarm(Date.now() + 30_000);
          }
          return true;
        } catch { this.close(sid); return false; }
        finally { clearTimeout(timer); }
      })();
      return await session.ready ? reply(200, null, { "X-Relay-Ranges": "1" }) : reply(502);
    }

    const session = this.sessions.get(sid);
    if (!session || session.closed) return reply(410);
    session.activity = Date.now();
    if (action === "close" && request.method === "POST") {
      this.close(sid);
      return reply(204);
    }
    if (action === "range" && request.method === "GET") {
      const offset = Number(url.searchParams.get("offset"));
      const size = Number(url.searchParams.get("size"));
      const ack = Number(url.searchParams.get("ack"));
      if (!["offset", "size", "ack"].every((key) => url.searchParams.has(key))
          || ![offset, size, ack].every(Number.isSafeInteger)
          || size < 1 || size > MAX_RANGE_BYTES || ack < 0 || offset < 0) return reply(400);
      if (session.flight || session.downSeq) return reply(409);
      session.rangeMode = true;
      if (ack > session.rangeAck + session.queued) return reply(409);
      if (ack > session.rangeAck) {
        this.take(session, ack - session.rangeAck);
        session.rangeAck = ack;
        session.downBytes = ack;
      }
      if (offset < session.rangeAck || offset + size > session.rangeAck + RANGE_LOOKAHEAD) return reply(409);
      const deadline = Date.now() + 10_000;
      while (!session.closed && !session.eof && offset >= session.rangeAck + session.queued) {
        if (session.rangeWaiters.size >= 8) return reply(429);
        let wake, timer;
        await new Promise((resolve) => {
          wake = resolve;
          session.rangeWaiters.add(wake);
          timer = setTimeout(resolve, Math.max(1, deadline - Date.now()));
        });
        clearTimeout(timer);
        session.rangeWaiters.delete(wake);
        if (Date.now() >= deadline) break;
      }
      if (session.closed) return reply(410);
      if (offset < session.rangeAck) return reply(409);
      if (offset < session.rangeAck + session.queued) {
        if (session.rangeAck + session.queued - offset < size && !session.eof) {
          await new Promise((resolve) => setTimeout(resolve, 5));
        }
        if (offset < session.rangeAck) return reply(409);
        const bytes = this.range(session, offset, size);
        return reply(200, bytes, { "X-Relay-Offset": String(offset), "X-Relay-Ranges": "1" });
      }
      if (session.eof) {
        return reply(410, null, { "X-Relay-End": String(session.rangeAck + session.queued), "X-Relay-Ranges": "1" });
      }
      return reply(204);
    }
    const seq = Number(url.searchParams.get("seq"));
    if (!url.searchParams.has("seq") || !Number.isSafeInteger(seq) || seq < 0) return reply(400);
    if (action === "up" && request.method === "POST") {
      let data;
      try { data = await bodyBytes(request, CHUNK_BYTES); } catch { return reply(413); }
      // Сервер подтверждает запись только после TCP write. Повтор HTTP-запроса
      // с тем же seq подтверждается без повторной отправки ciphertext в Telegram.
      const result = session.upTail.then(async () => {
        if (session.closed) return reply(410);
        if (seq < session.upSeq) {
          console.warn("relay-up-replay", sid.slice(0, 8), seq, session.upSeq);
          return reply(200);
        }
        if (seq !== session.upSeq) return reply(409);
        try {
          await session.writer.write(data);
          session.upSeq++;
          session.upBytes += data.byteLength;
          return reply(200);
        } catch { this.close(sid); return reply(410); }
      });
      session.upTail = result.then(() => {});
      return result;
    }
    if (action === "down" && request.method === "GET") {
      if (session.rangeMode) return reply(409);
      if (session.downBusy) return reply(409);
      session.downBusy = true;
      try {
        if (seq === session.downSeq + 1 && session.flight) {
          session.downSeq++;
          session.flight = null;
        }
        if (seq !== session.downSeq) return reply(409);
        if (!session.flight && !session.queued && !session.eof) {
          let timer;
          await new Promise((resolve) => {
            session.wakeData = resolve;
            timer = setTimeout(resolve, 10_000);
          });
          clearTimeout(timer);
          session.wakeData = null;
        }
        if (session.closed) return reply(410);
        if (!session.flight && session.queued) {
          // Собираем мелкие TCP reads, не превышая размер безопасного ответа.
          if (session.queued < CHUNK_BYTES && !session.eof) {
            await new Promise((resolve) => setTimeout(resolve, 5));
          }
          session.flight = this.take(session);
        }
        if (session.flight) {
          if (session.lastSent === seq) console.warn("relay-down-replay", sid.slice(0, 8), seq);
          else session.downBytes += session.flight.byteLength;
          session.lastSent = seq;
          if (seq > 0 && seq % 128 === 0) console.log("relay-progress", JSON.stringify({
            id: sid.slice(0, 8), dst: session.dst, seq, read: session.readBytes,
            delivered: session.downBytes, queued: session.queued, ageMs: Date.now() - session.created
          }));
          return reply(200, session.flight, { "X-Relay-Seq": String(seq) });
        }
        if (session.eof) { this.close(sid, session.readError || "tcp-eof"); return reply(410); }
        return reply(204);
      } finally { session.downBusy = false; }
    }
    return reply(404);
  }
}
