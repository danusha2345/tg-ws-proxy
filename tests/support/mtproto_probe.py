"""Live req_pq -> req_DH probe; never completes an auth key or uses an account.

Optional dependencies: telethon, pycryptodome, websockets>=15.
Run with an isolated Python environment; PYTHONPATH is also supported.
"""

import argparse
import asyncio
import hashlib
import os
import socket
import ssl
import struct
import time

import websockets
from Crypto.Cipher import AES as CTR
from telethon.crypto import AES, Factorization, rsa
from telethon.extensions import BinaryReader
from telethon.tl.functions import ReqDHParamsRequest, ReqPqMultiRequest
from telethon.tl.types import PQInnerDataTempDc, ResPQ, ServerDHParamsOk

# Production public key, fingerprint 0xd09d1d85de64fd85 (checked 2026-10-04).
# https://github.com/DrKLO/Telegram/blob/master/TMessagesProj/jni/tgnet/Handshake.cpp
rsa.add_key("""-----BEGIN RSA PUBLIC KEY-----
MIIBCgKCAQEA6LszBcC1LGzyr992NzE0ieY+BSaOW622Aa9Bd4ZHLl+TuFQ4lo4g
5nKaMBwK/BIb9xUfg0Q29/2mgIR6Zr9krM7HjuIcCzFvDtr+L0GQjae9H0pRB2OO
62cECs5HKhT5DZ98K33vmWiLowc621dQuwKWSQKjWf50XYFw42h21P2KXUGyp2y/
+aEyZ+uVgLLQbRA1dEjSDZ2iGRy12Mk5gpYc397aYp438fsJoHIgJ2lgMv5h7WY9
t6N/byY9Nw9p21Og3AoXSL2q/2IJ1WRUhebgAdGVMlV1fkuOQoEzR7EdpqtQD9Cs
5+bfo3Nhmcyvk5ftB0WkJ9z6bNZ7yxrP8wIDAQAB
-----END RSA PUBLIC KEY-----""", old=False)
SERVER_FINGERPRINT = 0xD09D1D85DE64FD85 - (1 << 64)
assert SERVER_FINGERPRINT in rsa._server_keys, "production RSA fingerprint mismatch"


def rsa_pad(fingerprint, data):
    """RSA_PAD for current Telegram server keys (not legacy SHA1 padding)."""
    key = rsa._server_keys[fingerprint][0]
    while True:
        padded = data + os.urandom(192 - len(data))
        temporary_key = os.urandom(32)
        encrypted = AES.encrypt_ige(
            padded[::-1] + hashlib.sha256(temporary_key + padded).digest(),
            temporary_key, bytes(32),
        )
        masked_key = bytes(a ^ b for a, b in zip(
            temporary_key, hashlib.sha256(encrypted).digest()))
        number = int.from_bytes(masked_key + encrypted, "big")
        if number < key.n:
            return pow(number, key.e, key.n).to_bytes(256, "big")


async def probe(domain, dc, ip):
    sock = socket.socket()
    sock.setblocking(False)
    try:
        await asyncio.wait_for(asyncio.get_running_loop().sock_connect(sock, (ip, 443)), 8)
        async with websockets.connect(
            "wss://" + domain + "/apiws", sock=sock,
            ssl=ssl.create_default_context(), server_hostname=domain,
            subprotocols=["binary"], proxy=None, open_timeout=8, ping_interval=None,
        ) as websocket:
            init = bytearray(os.urandom(64))
            init[0] = 0x31  # Avoid all reserved obfuscated transport prefixes.
            init[56:60] = bytes([0xDD]) * 4
            init[60:62] = struct.pack("<h", dc)
            encrypt = CTR.new(bytes(init[8:40]), CTR.MODE_CTR, nonce=b"",
                              initial_value=int.from_bytes(init[40:56], "big"))
            reverse = init[8:56][::-1]
            decrypt = CTR.new(bytes(reverse[:32]), CTR.MODE_CTR, nonce=b"",
                              initial_value=int.from_bytes(reverse[32:], "big"))
            encrypted_init = encrypt.encrypt(init)
            await websocket.send(bytes(init[:56]) + encrypted_init[56:])
            buffer = b""

            async def request(obj):
                nonlocal buffer
                body = bytes(obj)
                message_id = int(time.time() * 2**32) & ~3
                payload = bytes(8) + struct.pack("<QI", message_id, len(body)) + body
                payload += os.urandom(7)  # Padded intermediate transport.
                await websocket.send(encrypt.encrypt(struct.pack("<I", len(payload)) + payload))
                while True:
                    if len(buffer) >= 4:
                        size = struct.unpack("<I", buffer[:4])[0]
                        if not 4 <= size <= 4096:
                            raise ValueError("unexpected transport frame length")
                        if len(buffer) >= size + 4:
                            data, buffer = buffer[4:4 + size], buffer[size + 4:]
                            error = struct.unpack("<i", data[:4])[0]
                            if len(data) <= 56 and error < 0:
                                return error
                            length = struct.unpack("<I", data[16:20])[0]
                            with BinaryReader(data[20:20 + length]) as reader:
                                return reader.tgread_object()
                    data = await asyncio.wait_for(websocket.recv(), 8)
                    buffer += decrypt.decrypt(data)

            nonce = int.from_bytes(os.urandom(16), "little", signed=True)
            pq = await request(ReqPqMultiRequest(nonce))
            if isinstance(pq, int):
                raise ValueError(f"req_pq failed with transport error {pq}")
            assert isinstance(pq, ResPQ) and pq.nonce == nonce, "unexpected resPQ"
            p, q = Factorization.factorize(int.from_bytes(pq.pq, "big"))
            p, q = rsa.get_byte_array(p), rsa.get_byte_array(q)
            fingerprint = next(x for x in pq.server_public_key_fingerprints if x == SERVER_FINGERPRINT)
            inner = bytes(PQInnerDataTempDc(
                pq=pq.pq, p=p, q=q, nonce=nonce, server_nonce=pq.server_nonce,
                new_nonce=int.from_bytes(os.urandom(32), "little", signed=True),
                dc=dc, expires_in=3600,
            ))
            answer = await request(ReqDHParamsRequest(
                nonce=nonce, server_nonce=pq.server_nonce, p=p, q=q,
                public_key_fingerprint=fingerprint, encrypted_data=rsa_pad(fingerprint, inner),
            ))
            if isinstance(answer, ServerDHParamsOk):
                assert answer.nonce == nonce and answer.server_nonce == pq.server_nonce
                return "ServerDHParamsOk"
            return str(answer) if isinstance(answer, int) else type(answer).__name__
    finally:
        sock.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--domain", required=True)
    parser.add_argument("--dc", required=True, type=int)
    parser.add_argument("--ip", default="149.154.167.220")
    parser.add_argument("--expect", required=True)
    args = parser.parse_args()
    result = asyncio.run(asyncio.wait_for(probe(args.domain, args.dc, args.ip), 30))
    print(f"domain={args.domain} dc={args.dc} req_DH={result}")
    if result != args.expect:
        raise SystemExit(f"expected {args.expect}, received {result}")
