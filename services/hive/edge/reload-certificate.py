#!/usr/bin/env python3
"""Reload this installation's edge only when its externally managed certificate changes."""
import argparse
import fcntl
import hashlib
import os
from pathlib import Path
import socket
import ssl
import subprocess
import time


def run(*args):
    return subprocess.run(args, check=True, capture_output=True, timeout=15).stdout


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--container', required=True)
    parser.add_argument('--hostname', required=True)
    parser.add_argument('--port', type=int, required=True)
    parser.add_argument('--certificate', type=Path, required=True)
    parser.add_argument('--state', type=Path, required=True)
    args = parser.parse_args()
    if not 1024 <= args.port <= 65535:
        parser.error('Use the explicitly assigned separate HTTPS port.')
    with args.state.with_suffix('.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        expected = hashlib.sha256(run('openssl', 'x509', '-in', str(args.certificate), '-outform', 'DER')).hexdigest()
        if args.state.exists() and args.state.read_text().strip() == expected:
            return
        run('openssl', 'x509', '-in', str(args.certificate), '-checkend', '86400', '-noout')
        run('docker', 'exec', args.container, 'caddy', 'validate', '--config', '/etc/caddy/Caddyfile', '--adapter', 'caddyfile')
        run('docker', 'exec', args.container, 'caddy', 'reload', '--force', '--address', 'unix//config/admin.sock', '--config', '/etc/caddy/Caddyfile', '--adapter', 'caddyfile')
        context = ssl.create_default_context()
        for attempt in range(10):
            with socket.create_connection(('127.0.0.1', args.port), timeout=3) as connection:
                with context.wrap_socket(connection, server_hostname=args.hostname) as secured:
                    actual = hashlib.sha256(secured.getpeercert(binary_form=True)).hexdigest()
            if actual == expected:
                temporary = args.state.with_suffix('.tmp')
                temporary.write_text(expected + '\n')
                os.replace(temporary, args.state)
                print('Reloaded and verified the current certificate on the owned edge.')
                return
            time.sleep(0.5)
        raise RuntimeError('The served certificate did not match the expected certificate.')


if __name__ == '__main__':
    main()
