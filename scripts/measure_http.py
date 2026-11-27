import argparse
import http.client
import json
import math
import socket
import sys
import time
from urllib.parse import urlsplit


DIRECTIONS = {"up", "down", "left", "right"}


class FixedAddressHTTPSConnection(http.client.HTTPSConnection):
    """Connect to a relay IP while preserving the URL host for TLS SNI."""

    def __init__(self, host, connect_ip, port=None, **kwargs):
        super().__init__(host, port=port, **kwargs)
        self.connect_ip = connect_ip

    def connect(self):
        self.sock = socket.create_connection(
            (self.connect_ip, self.port), self.timeout, self.source_address
        )
        self.sock = self._context.wrap_socket(self.sock, server_hostname=self.host)


def request(conn, method, path, body=None):
    headers = {"Content-Type": "application/json"} if body is not None else {}
    conn.request(method, path, body=body, headers=headers)
    response = conn.getresponse()
    payload = response.read()
    if response.status != 200:
        raise RuntimeError(f"{method} {path} returned {response.status}: {payload[:256]!r}")
    return payload


def main():
    parser = argparse.ArgumentParser(description="Time Battlesnake webhooks from JSON frames on stdin")
    parser.add_argument("url")
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument(
        "--connect-ip",
        help="connect to this IP while retaining the HTTPS host for TLS and Host headers",
    )
    args = parser.parse_args()
    url = urlsplit(args.url)
    if url.scheme not in {"https", "http"} or not url.hostname or args.repeats < 1:
        parser.error("supply an HTTP or HTTPS URL and a positive repeat count")
    if args.connect_ip and url.scheme != "https":
        parser.error("--connect-ip currently supports HTTPS URLs only")

    frames = json.load(sys.stdin)
    if not frames:
        parser.error("expected a JSON array of Battlesnake move requests on stdin")
    example = json.dumps(frames[0], separators=(",", ":")).encode()
    path = url.path.rstrip("/")
    if args.connect_ip:
        conn = FixedAddressHTTPSConnection(
            url.hostname, args.connect_ip, url.port, timeout=5
        )
    else:
        connection_type = (
            http.client.HTTPSConnection
            if url.scheme == "https"
            else http.client.HTTPConnection
        )
        conn = connection_type(url.hostname, url.port, timeout=5)
    try:
        info = json.loads(request(conn, "GET", path + "/"))
        if info.get("apiversion") != "1":
            raise RuntimeError(f"unexpected metadata: {info}")
        request(conn, "POST", path + "/start", example)
        elapsed = []
        missed = 0
        for _ in range(args.repeats):
            for frame in frames:
                body = json.dumps(frame, separators=(",", ":")).encode()
                started = time.perf_counter_ns()
                raw = request(conn, "POST", path + "/move", body)
                duration = (time.perf_counter_ns() - started) / 1_000_000
                if json.loads(raw).get("move") not in DIRECTIONS:
                    raise RuntimeError(f"invalid move: {raw!r}")
                elapsed.append(duration)
                missed += duration >= frame["game"]["timeout"]
        request(conn, "POST", path + "/end", example)
    finally:
        conn.close()

    elapsed.sort()

    def percentile(p):
        return elapsed[math.ceil(len(elapsed) * p) - 1]

    print(
        f"{len(elapsed)} moves, p50 {percentile(0.50):.2f} ms, "
        f"p95 {percentile(0.95):.2f} ms, p99 {percentile(0.99):.2f} ms, "
        f"max {elapsed[-1]:.2f} ms, {missed} at or beyond timeout"
    )
    return int(missed > 0)


if __name__ == "__main__":
    sys.exit(main())
