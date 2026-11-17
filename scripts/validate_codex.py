#!/usr/bin/env python3
"""Exercise the installed Codex client through ShrinkGate with a local Responses fixture.
Uses only a dummy key and isolated configuration; no live-provider/auth/billing claim.
"""
import argparse
import hashlib
import http.client
import http.server
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time

class Provider(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    observations = []
    fail_auth = False

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        assert self.headers.get('Authorization') == 'Bearer local-fixture-only'
        self.observations.append({'path': self.path, 'stream': body.get('stream'), 'model': body.get('model'), 'status': 401 if self.fail_auth else 200})
        if self.fail_auth:
            self.reply(401, b'{"error":{"message":"fixture authentication rejected","type":"invalid_api_key"}}', 'application/json')
            return
        item = {'id': 'msg_fixture', 'type': 'message', 'role': 'assistant', 'status': 'completed', 'content': [{'type': 'output_text', 'text': 'SHRINKGATE_CLIENT_PROBE_OK', 'annotations': []}]}
        response = {'id': 'resp_fixture', 'object': 'response', 'created_at': 1, 'status': 'completed', 'model': 'fixture-model', 'output': [item], 'usage': {'input_tokens': 100, 'output_tokens': 7, 'total_tokens': 107, 'input_tokens_details': {'cached_tokens': 0}, 'output_tokens_details': {'reasoning_tokens': 0}}}
        location = {'item_id': 'msg_fixture', 'output_index': 0, 'content_index': 0}
        events = [('response.created', {'response': {**response, 'status': 'in_progress', 'output': []}}), ('response.output_item.added', {'output_index': 0, 'item': {**item, 'status': 'in_progress', 'content': []}}), ('response.content_part.added', {**location, 'part': {'type': 'output_text', 'text': '', 'annotations': []}}), ('response.output_text.delta', {**location, 'delta': 'SHRINKGATE_CLIENT_PROBE_OK'}), ('response.output_text.done', {**location, 'text': 'SHRINKGATE_CLIENT_PROBE_OK'}), ('response.output_item.done', {'output_index': 0, 'item': item}), ('response.completed', {'response': response})]
        data = ''.join(('event: ' + name + '\n' + 'data: ' + json.dumps({'type': name, 'sequence_number': i, **value}) + '\n\n' for i, (name, value) in enumerate(events))).encode()
        self.reply(200, data, 'text/event-stream')

    def reply(self, status, data, content_type):
        self.send_response(status)
        self.send_header('Content-Type', content_type)
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *_):
        pass

def run_client(command, environment):
    result = subprocess.run(command, env=environment, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60)
    events = []
    for line in result.stdout.splitlines():
        try:
            events.append(json.loads(line))
        except ValueError:
            pass
    return (result.returncode, events)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=Path('target/release/shrinkgate'))
    parser.add_argument('--codex', default='codex')
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    binary = str(args.binary.resolve())
    version = subprocess.check_output([args.codex, '--version'], text=True, stderr=subprocess.DEVNULL).strip()
    provider = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    provider.daemon_threads = True
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    with tempfile.TemporaryDirectory(prefix='shrinkgate-codex-') as directory:
        root = Path(directory)
        (root / 'codex').mkdir()
        config = root / 'config/shrinkgate'
        config.mkdir(parents=True)
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            port = listener.getsockname()[1]
        upstream = f'http://127.0.0.1:{provider.server_port}'
        (config / 'config.toml').write_text(f'port = {port}\nupstream_openai = "{upstream}"\n')
        original = 'model = "fixture-model"\n'
        (root / 'codex/config.toml').write_text(original)
        environment = {**os.environ, 'CODEX_HOME': str(root / 'codex'), 'USERPROFILE': str(root), 'XDG_CONFIG_HOME': str(root / 'config'), 'XDG_STATE_HOME': str(root / 'state'), 'SHRINKGATE_TEST_KEY': 'local-fixture-only'}
        command = [args.codex, 'exec', '--ephemeral', '--skip-git-repo-check', '--sandbox', 'read-only', '--json', '--color', 'never', '-C', str(root)]
        prompt = 'Reply exactly SHRINKGATE_CLIENT_PROBE_OK. Do not use any tools.'
        direct_overrides = ['-c', 'model_provider="probe"', '-c', 'model_providers.probe.name="Local fixture"', '-c', f'model_providers.probe.base_url="{upstream}/v1"', '-c', 'model_providers.probe.wire_api="responses"', '-c', 'model_providers.probe.env_key="SHRINKGATE_TEST_KEY"']
        direct_code, direct_events = run_client(command + direct_overrides + [prompt], environment)
        assert direct_code == 0 and any((e.get('type') == 'turn.completed' for e in direct_events)), 'direct fixture failed'
        engine = subprocess.Popen([binary, 'serve', '--quiet'], env=environment, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            for _ in range(250):
                try:
                    connection = http.client.HTTPConnection('127.0.0.1', port, timeout=1)
                    connection.request('GET', '/healthz')
                    response = connection.getresponse()
                    healthy = response.status == 200
                    response.read()
                    connection.close()
                    if healthy:
                        break
                except (OSError, http.client.HTTPException):
                    pass
                time.sleep(0.02)
            else:
                raise RuntimeError('gateway did not start')
            subprocess.run([binary, 'connect', 'codex', '--env-key', 'SHRINKGATE_TEST_KEY', '--no-service'], env=environment, check=True, stdout=subprocess.DEVNULL)
            gateway_code, gateway_events = run_client(command + [prompt], environment)
            assert gateway_code == 0 and any((e.get('type') == 'turn.completed' for e in gateway_events)), 'wired fixture failed'
            messages = [e.get('item', {}).get('text') for e in gateway_events if e.get('type') == 'item.completed']
            assert 'SHRINKGATE_CLIENT_PROBE_OK' in messages, 'fixture answer was not preserved'
            Provider.fail_auth = True
            failure_code, failure_events = run_client(command + [prompt], environment)
            assert failure_code != 0 and any((e.get('type') == 'turn.failed' for e in failure_events)), '401 did not fail client turn'
            summary = json.loads(subprocess.check_output([binary, 'stats', '--json'], env=environment))
            assert summary['failed_or_cancelled_requests'] >= 1, 'upstream failures were not journaled'
            subprocess.run([binary, 'uninstall', 'codex'], env=environment, check=True, stdout=subprocess.DEVNULL)
            assert (root / 'codex/config.toml').read_text() == original, 'configuration not restored'
            report = {'kind': 'actual installed client with loopback provider fixture; not live-provider validation', 'client': version, 'gateway': subprocess.check_output([binary, '--version'], text=True).strip(), 'gateway_binary_sha256': hashlib.sha256(Path(binary).read_bytes()).hexdigest(), 'date': time.strftime('%Y-%m-%d', time.gmtime()), 'dummy_auth_header_forwarded': True, 'direct_fixture_turn': 'passed', 'wired_fixture_turn': 'passed', 'streamed_answer_preserved': True, 'upstream_401_fails_client_turn': True, 'connect_uninstall_restores_config': True, 'observed_requests': Provider.observations, 'journal_summary': summary}
        finally:
            engine.terminate()
            engine.wait(timeout=15)
    provider.shutdown()
    text = json.dumps(report, indent=2) + '\n'
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(text)
    print(text)
if __name__ == '__main__':
    main()
