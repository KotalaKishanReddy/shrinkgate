#!/usr/bin/env python3
"""Bounded offline gateway soak: three APIs, SSE, errors, cancellation and restart.
All traffic stays on loopback. No provider keys, model calls or billing claims.
"""
import argparse
import hashlib
import concurrent.futures
import http.client
import http.server
import json
import os
from pathlib import Path
import platform
import socket
import subprocess
import tempfile
import threading
import time


class Provider(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    observations = {"transformed": 0, "errors": 0, "cancelled": 0}
    lock = threading.Lock()

    def do_POST(self):
        raw = self.rfile.read(int(self.headers["Content-Length"]))
        body = json.loads(raw)
        assert self.headers.get("Authorization") == "Bearer local-fixture-only"
        assert self.path in ("/v1/messages", "/v1/chat/completions", "/v1/responses")
        # The protected latest copy and earlier stubs must both reach upstream.
        assert "[shrinkgate]" in raw.decode()
        assert "validation-payload" in raw.decode()
        assert body.get("model") == "fixture-model"
        mode = self.headers.get("x-validation-mode")
        with self.lock:
            self.observations["transformed"] += 1
        if mode == "error":
            with self.lock:
                self.observations["errors"] += 1
            self.reply(503, b'{"error":{"message":"injected fixture failure"}}')
        elif mode == "cancel":
            with self.lock:
                self.observations["cancelled"] += 1
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Connection", "close")
            self.end_headers()
            try:
                self.wfile.write(b'data: {"type":"ping"}\n\n')
                self.wfile.flush()
                time.sleep(0.15)
                self.wfile.write(b'data: {"usage":{"input_tokens":100}}\n\n')
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                pass
            self.close_connection = True
        else:
            self.reply(200, b'data: {"usage":{"input_tokens":100,"output_tokens":7}}\n\ndata: [DONE]\n\n', "text/event-stream")

    def reply(self, status, data, content_type="application/json"):
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *_):
        pass


def fixture(kind, worker):
    text = ("validation-payload: compiler diagnostic at src/example.rs:42\n" * 64)
    messages = [{"role": "user", "content": "Inspect the compiler output"}]
    items = [{"role": "user", "content": [{"type": "input_text", "text": "Inspect the compiler output"}]}]
    for i in range(32):
        call_id = f"worker-{worker}-call-{i}"
        output = text if i < 31 else text.replace(":42", ":43")
        if kind == "anthropic":
            messages += [{"role": "assistant", "content": [{"type": "tool_use", "id": call_id, "name": "build", "input": {}}]},
                         {"role": "user", "content": [{"type": "tool_result", "tool_use_id": call_id, "content": output}]}]
        elif kind == "chat":
            messages += [{"role": "assistant", "tool_calls": [{"id": call_id, "type": "function", "function": {"name": "build", "arguments": "{}"}}]},
                         {"role": "tool", "tool_call_id": call_id, "content": output}]
        else:
            items += [{"type": "function_call", "call_id": call_id, "name": "build", "arguments": "{}"},
                      {"type": "function_call_output", "call_id": call_id, "output": output}]
    body = {"model": "fixture-model", "stream": True}
    if kind == "responses":
        body["input"] = items
    else:
        body["messages"] = messages
        if kind == "anthropic":
            body["max_tokens"] = 64
            body["metadata"] = {"user_id": f"soak-worker-{worker}"}
    return json.dumps(body).encode()


def free_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def rss_kib(pid):
    try:
        for line in Path(f"/proc/{pid}/status").read_text().splitlines():
            if line.startswith("VmRSS:"):
                return int(line.split()[1])
    except OSError:
        pass
    return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/release/shrinkgate"))
    parser.add_argument("--duration", type=float, default=600)
    parser.add_argument("--workers", type=int, default=8)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if args.duration < 2 or not 1 <= args.workers <= 8:
        parser.error("duration must be >= 2 and workers between 1 and 8")
    binary = args.binary.resolve()
    version = subprocess.check_output([str(binary), "--version"], text=True).strip()
    provider = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    provider.daemon_threads = True
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    phases = []
    errors = []
    started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="shrinkgate-soak-") as directory:
        root = Path(directory)
        config = root / "config/shrinkgate"
        config.mkdir(parents=True)
        port = free_port()
        upstream = f"http://127.0.0.1:{provider.server_port}"
        (config / "config.toml").write_text(f'port = {port}\nupstream_openai = "{upstream}"\nupstream_anthropic = "{upstream}"\n')
        environment = {**os.environ, "USERPROFILE": str(root), "XDG_CONFIG_HOME": str(root / "config"), "XDG_STATE_HOME": str(root / "state")}
        journal_dir = root / "state/shrinkgate"
        for phase in range(2):
            process = subprocess.Popen([str(binary), "serve", "--quiet"], env=environment, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            try:
                for _ in range(250):
                    try:
                        client = http.client.HTTPConnection("127.0.0.1", port, timeout=1)
                        client.request("GET", "/healthz")
                        response = client.getresponse()
                        healthy = response.status == 200 and json.loads(response.read())["engine"] == "shrinkgate"
                        client.close()
                        if healthy:
                            break
                    except (OSError, http.client.HTTPException):
                        pass
                    time.sleep(0.02)
                else:
                    raise RuntimeError("gateway did not become healthy")
                deadline = time.monotonic() + args.duration / 2
                samples = [rss_kib(process.pid)]

                def worker(index):
                    bodies = {kind: fixture(kind, index) for kind in ("anthropic", "chat", "responses")}
                    routes = {"anthropic": "/v1/messages", "chat": "/v1/chat/completions", "responses": "/v1/responses"}
                    counts = {"success": 0, "expected_provider_errors": 0, "intentional_cancellations": 0}
                    client = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
                    i = index
                    try:
                        while time.monotonic() < deadline:
                            kind = ("anthropic", "chat", "responses")[i % 3]
                            mode = "cancel" if i % 61 == 0 else "error" if i % 43 == 0 else "success"
                            client.request("POST", routes[kind], bodies[kind], {"Content-Type": "application/json", "Authorization": "Bearer local-fixture-only", "x-validation-mode": mode})
                            response = client.getresponse()
                            if mode == "cancel":
                                assert response.status == 200
                                assert response.read(1) == b"d"
                                response.close()
                                client.close()
                                client = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
                                counts["intentional_cancellations"] += 1
                            else:
                                data = response.read()
                                assert response.status == (503 if mode == "error" else 200), response.status
                                if mode == "error":
                                    assert json.loads(data)["error"]["message"] == "injected fixture failure"
                                    counts["expected_provider_errors"] += 1
                                else:
                                    assert b'"input_tokens":100' in data and data.endswith(b"[DONE]\n\n")
                                    counts["success"] += 1
                            i += 1
                    except Exception as error:
                        errors.append({"phase": phase + 1, "worker": index, "type": type(error).__name__, "detail": str(error)[:100]})
                    finally:
                        client.close()
                    return counts

                with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as executor:
                    futures = [executor.submit(worker, i) for i in range(args.workers)]
                    while any(not future.done() for future in futures):
                        samples.append(rss_kib(process.pid))
                        concurrent.futures.wait(futures, timeout=1)
                    counts = [future.result() for future in futures]
                time.sleep(0.3)  # allow intentionally cancelled streams and journal writes to settle
                summary = json.loads(subprocess.check_output([str(binary), "stats", "--json"], env=environment))
                valid_samples = [value for value in samples if value is not None]
                phases.append({"phase": phase + 1, **{key: sum(c[key] for c in counts) for key in counts[0]},
                               "rss_start_kib": valid_samples[0] if valid_samples else None,
                               "rss_end_kib": valid_samples[-1] if valid_samples else None,
                               "rss_peak_kib": max(valid_samples) if valid_samples else None,
                               "retained_journal_requests": summary["requests"],
                               "retained_failed_or_cancelled": summary["failed_or_cancelled_requests"]})
            finally:
                process.terminate()
                process.wait(timeout=15)
                assert process.returncode == 0, f"shutdown exit {process.returncode}"
        sizes = {file.name: file.stat().st_size for file in journal_dir.glob("usage*.jsonl")}
        assert all(size <= (8 << 20) + 65536 for size in sizes.values()), sizes
        observed_cancelled = 0
        for file in journal_dir.glob("usage*.jsonl"):
            for line in file.read_text().splitlines():
                if json.loads(line)["status"] == 499:
                    observed_cancelled += 1
        assert observed_cancelled > 0, "cancelled streams were not journaled"
        if args.duration >= 600:
            assert "usage.previous.jsonl" in sizes, "long soak did not exercise journal rotation"
        assert not errors, errors
    provider.shutdown()
    report = {"kind": "offline loopback soak; not live-agent, billing or multi-hour certification",
              "version": version, "gateway_binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "date": time.strftime("%Y-%m-%d", time.gmtime()), "platform": platform.system(),
              "requested_active_seconds": args.duration, "elapsed_seconds": round(time.monotonic() - started, 2),
              "workers": args.workers, "protocols": ["anthropic_messages", "chat_completions", "responses"],
              "history": "32 paired tool calls/results per request; repeated outputs plus changed tail",
              "restart_count": 1, "phases": phases, "unexpected_failures": errors,
              "upstream_observations": Provider.observations, "journal_sizes_bytes": sizes,
              "cancelled_records_in_retained_history": observed_cancelled}
    encoded = json.dumps(report, indent=2) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(encoded)
    print(encoded)


if __name__ == "__main__":
    main()
