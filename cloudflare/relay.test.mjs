import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

let sockets = [];
globalThis.__relayTestConnect = () => {
  let controller;
  const socket = {
    opened: Promise.resolve(),
    writes: [],
    readable: new ReadableStream({ start(value) { controller = value; } }),
    writable: new WritableStream({ write(data) { socket.writes.push(new Uint8Array(data)); } }),
    feed(data) { controller.enqueue(data); },
    close() { try { controller.close(); } catch {} },
  };
  sockets.push(socket);
  return socket;
};
const source = (await readFile(new URL("./relay.mjs", import.meta.url), "utf8"))
  .replace('import { connect } from "cloudflare:sockets";', "const connect = globalThis.__relayTestConnect;");
const { RelayHub } = await import("data:text/javascript;base64," + Buffer.from(source).toString("base64"));
const SID = "0123456789abcdef0123456789abcdef";
const INIT = new Uint8Array(64).fill(31);

function setup(t) {
  sockets = [];
  const hub = new RelayHub({ storage: { async setAlarm() {} } });
  t.after(() => { for (const sid of hub.sessions.keys()) hub.close(sid); });
  return hub;
}
function req(action, query = "", body) {
  return new Request(`https://worker.test/relay/${action}?sid=${SID}${query}`, {
    method: action === "down" ? "GET" : "POST", body,
  });
}
async function open(hub) {
  const result = await hub.fetch(req("open", "&dst=149.154.167.51", INIT));
  assert.equal(result.status, 200);
  assert.equal(result.headers.get("x-tg-relay"), "1");
  return sockets[0];
}

test("open retries do not duplicate the Telegram init or create another TCP socket", async (t) => {
  const hub = setup(t);
  const socket = await open(hub);
  await open(hub);
  assert.equal(sockets.length, 1);
  assert.equal(socket.writes.length, 1);
  assert.deepEqual(socket.writes[0], INIT);
  const changed = new Uint8Array(INIT); changed[0]++;
  assert.equal((await hub.fetch(req("open", "&dst=149.154.167.51", changed))).status, 409);
});

test("upload retries are idempotent and out-of-order or oversized chunks are rejected", async (t) => {
  const hub = setup(t);
  const socket = await open(hub);
  assert.equal((await hub.fetch(req("up", "&seq=1", new Uint8Array([9])))).status, 409);
  assert.equal((await hub.fetch(req("up", "&seq=0", new Uint8Array([1, 2])))).status, 200);
  assert.equal((await hub.fetch(req("up", "&seq=0", new Uint8Array([1, 2])))).status, 200);
  assert.equal(socket.writes.length, 2);
  assert.equal((await hub.fetch(req("up", "&seq=1", new Uint8Array(8193)))).status, 413);
});

test("download retries replay identical bytes; acknowledgement releases exactly one bounded chunk", async (t) => {
  const hub = setup(t);
  const socket = await open(hub);
  const data = new Uint8Array(20000).map((_, i) => i % 251);
  socket.feed(data);
  const first = await hub.fetch(req("down", "&seq=0"));
  const retry = await hub.fetch(req("down", "&seq=0"));
  assert.deepEqual(new Uint8Array(await first.arrayBuffer()), data.slice(0, 8192));
  assert.deepEqual(new Uint8Array(await retry.arrayBuffer()), data.slice(0, 8192));
  const second = await hub.fetch(req("down", "&seq=1"));
  assert.deepEqual(new Uint8Array(await second.arrayBuffer()), data.slice(8192, 16384));
  const third = await hub.fetch(req("down", "&seq=2"));
  assert.deepEqual(new Uint8Array(await third.arrayBuffer()), data.slice(16384));
  assert.equal((await hub.fetch(req("down", "&seq=0"))).status, 409);
});

test("destinations, session isolation and idle cleanup are enforced", async (t) => {
  const hub = setup(t);
  assert.equal((await hub.fetch(req("open", "&dst=127.0.0.1", INIT))).status, 403);
  await open(hub);
  const other = req("down", "&seq=0");
  assert.equal((await hub.fetch(new Request(other.url.replace(SID, "a".repeat(32))))).status, 410);
  hub.sessions.get(SID).activity -= 100000;
  await hub.alarm();
  assert.equal(hub.sessions.size, 0);
  assert.equal((await hub.fetch(req("down", "&seq=0"))).status, 410);
});

function rangeRequest(offset, size, ack = 0) {
  return new Request(`https://worker.test/relay/range?sid=${SID}&offset=${offset}&size=${size}&ack=${ack}`);
}

test("ranges can be requested out of order and retried with a smaller size", async (t) => {
  const hub = setup(t);
  const socket = await open(hub);
  const data = new Uint8Array(60000).map((_, i) => i % 251);
  socket.feed(data);
  const [later, first, smaller] = await Promise.all([
    hub.fetch(rangeRequest(16384, 8192)),
    hub.fetch(rangeRequest(0, 32768)),
    hub.fetch(rangeRequest(0, 8192)),
  ]);
  assert.deepEqual(new Uint8Array(await later.arrayBuffer()), data.slice(16384, 24576));
  assert.deepEqual(new Uint8Array(await first.arrayBuffer()), data.slice(0, 32768));
  assert.deepEqual(new Uint8Array(await smaller.arrayBuffer()), data.slice(0, 8192));
  const next = await hub.fetch(rangeRequest(8192, 12288, 8192));
  assert.deepEqual(new Uint8Array(await next.arrayBuffer()), data.slice(8192, 20480));
  assert.equal(hub.sessions.get(SID).rangeAck, 8192);
  assert.equal((await hub.fetch(rangeRequest(0, 8192))).status, 409);
  assert.equal((await hub.fetch(rangeRequest(8192, 32769, 8192))).status, 400);
  assert.equal((await hub.fetch(rangeRequest(1000000, 8192, 8192))).status, 409);
});

test("a future range waiting for data does not block an available prefix", async (t) => {
  const hub = setup(t);
  const socket = await open(hub);
  const future = hub.fetch(rangeRequest(8192, 8192));
  socket.feed(new Uint8Array([11, 12, 13]));
  const first = await hub.fetch(rangeRequest(0, 8192));
  assert.deepEqual(new Uint8Array(await first.arrayBuffer()), new Uint8Array([11, 12, 13]));
  hub.close(SID);
  assert.equal((await future).status, 410);
});

test("EOF reports the byte offset without discarding earlier pending ranges", async (t) => {
  const hub = setup(t);
  const socket = await open(hub);
  const data = new Uint8Array(10000).fill(7);
  socket.feed(data);
  socket.close();
  const end = await hub.fetch(rangeRequest(20000, 8192));
  assert.equal(end.status, 410);
  assert.equal(end.headers.get("x-relay-end"), "10000");
  const first = await hub.fetch(rangeRequest(0, 8192));
  assert.deepEqual(new Uint8Array(await first.arrayBuffer()), data.slice(0, 8192));
  const last = await hub.fetch(rangeRequest(8192, 8192, 8192));
  assert.deepEqual(new Uint8Array(await last.arrayBuffer()), data.slice(8192));
});
