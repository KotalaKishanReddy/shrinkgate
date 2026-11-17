#!/usr/bin/env python3
"""Record real CLI output as a terminal SVG and asciinema replay. No API calls.
Replay output is paced line-by-line for readability, not original wall-clock timing.
"""
import html
import json
from pathlib import Path
import subprocess
import sys
import time

root = Path(__file__).resolve().parents[1]
binary = Path(sys.argv[1]) if len(sys.argv) > 1 else root / "target/release/shrinkgate"
result = subprocess.run([str(binary), "demo"], text=True, capture_output=True, check=True)
# Port numbers and temporary paths vary per run; keep the fixture result rows.
lines = ["$ shrinkgate demo", "Synthetic loopback fixture · no API keys"]
lines += [line for line in result.stdout.splitlines() if line.startswith(("request", "turn ", "Total estimated"))]
lines += ["Estimates = candidate bytes / 4. Not measured billing savings."]
width = max(1050, max(map(len, lines)) * 10 + 64)
height = 110 + len(lines) * 30
svg = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" role="img" aria-label="Recorded local ShrinkGate demo; synthetic fixture, not measured billing savings">',
       f'<rect width="{width}" height="{height}" rx="12" fill="#131f2d"/>',
       '<circle cx="26" cy="26" r="6" fill="#fb7185"/><circle cx="48" cy="26" r="6" fill="#fbbf24"/><circle cx="70" cy="26" r="6" fill="#93e5c2"/>']
for i, line in enumerate(lines):
    color = "#93e5c2" if i == 0 else "#e9eef6"
    svg.append(f'<text x="26" y="{80+i*30}" fill="{color}" font-family="monospace" font-size="16" xml:space="preserve">{html.escape(line)}</text>')
svg.append('</svg>')
(root / 'docs/demo.svg').write_text('\n'.join(svg) + '\n')
header = {"version": 2, "width": width // 10, "height": len(lines) + 3, "timestamp": int(time.time()), "title": "ShrinkGate synthetic local fixture; paced replay"}
events = [json.dumps(header)]
events += [json.dumps([i * 0.75, "o", line + "\r\n"], ensure_ascii=False) for i, line in enumerate(lines)]
(root / 'docs/demo.cast').write_text('\n'.join(events) + '\n')
print(result.stdout)
