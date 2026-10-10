#!/usr/bin/env python3
"""Test a real backend through an isolated Wayland connection; no real input."""
import argparse
import os
from pathlib import Path
import select
import subprocess
import sys
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cargo', default='cargo')
    parser.add_argument('--timeout', type=int, default=120)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    env = os.environ.copy()
    upstream = Path(env.get('WAYLAND_DISPLAY', 'wayland-0'))
    if not upstream.is_absolute():
        upstream = Path(env['XDG_RUNTIME_DIR']) / upstream
    if not upstream.is_socket():
        parser.error('WAYLAND_DISPLAY does not identify a live compositor socket')
    with tempfile.TemporaryDirectory(prefix='lan-mouse-recovery-') as tmp:
        endpoint = Path(tmp) / 'wayland.sock'
        flag = Path(tmp) / 'stall.flag'
        proxy = subprocess.Popen(
            [sys.executable, str(Path(__file__).with_name('proxy.py')),
             str(endpoint), str(upstream), str(flag)],
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
        )
        try:
            if not select.select([proxy.stdout], [], [], 3)[0]:
                raise RuntimeError('Timed out starting the isolated Wayland proxy')
            ready = proxy.stdout.readline().strip()
            if not ready.startswith('READY isolated Wayland stall proxy'):
                raise RuntimeError('Wayland proxy did not start: ' + ready)
            env['WAYLAND_DISPLAY'] = str(endpoint)
            env['LAN_MOUSE_STALL_FLAG'] = str(flag)
            result = subprocess.run(
                [args.cargo, 'test', '--locked', '--manifest-path', str(root / 'Cargo.toml'),
                 '--lib', '--no-default-features', '--features', 'wlroots_emulation',
                 'real_wayland_stall_disables_then_rebuilds_emulation',
                 '--', '--ignored', '--nocapture'],
                env=env, timeout=args.timeout,
            )
            return result.returncode
        finally:
            proxy.terminate()
            try:
                output, _ = proxy.communicate(timeout=3)
            except subprocess.TimeoutExpired:
                proxy.kill()
                output, _ = proxy.communicate()
            print(output.strip())


if __name__ == '__main__':
    raise SystemExit(main())
