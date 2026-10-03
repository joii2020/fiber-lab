#!/usr/bin/env python3
"""Local Fiber + hosted LSP orchestration (Linux, Python standard library)."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[1]


def load(name):
    path = ROOT / 'profiles' / (name + '.json')
    cfg = json.loads(path.read_text())
    nodes = cfg['nodes']
    if [n['id'] for n in nodes] != list(range(1, len(nodes) + 1)) or not 1 <= len(nodes) <= 16:
        raise ValueError('nodes must have consecutive ids starting at 1 (maximum 16)')
    return cfg


def resolve(value):
    return Path(os.path.expandvars(value)).expanduser().resolve() if Path(os.path.expandvars(value)).expanduser().is_absolute() else (ROOT / os.path.expandvars(value)).resolve()


def environment(state):
    env = os.environ.copy()
    for name, folder in [('XDG_DATA_HOME', 'data'), ('XDG_CONFIG_HOME', 'config'), ('XDG_CACHE_HOME', 'cache'), ('XDG_STATE_HOME', 'logs')]:
        env[name] = str(state / folder)
    return env


def off(cfg, env, *args):
    entry = resolve(cfg['offckb_entry'])
    if not entry.is_file():
        raise RuntimeError('offckb source build missing; run scripts/setup-offckb.sh')
    return subprocess.run(['node', str(entry), *args], env=env, check=True)


def rpc(url, method, params=None, token=None):
    headers = {'Content-Type': 'application/json'}
    if token:
        headers['Authorization'] = 'Bearer ' + token
    req = urllib.request.Request(url, json.dumps({'id': 1, 'jsonrpc': '2.0', 'method': method, 'params': params or []}).encode(), headers)
    with urllib.request.urlopen(req, timeout=3) as response:
        data = json.load(response)
    if 'error' in data:
        raise RuntimeError(str(data['error']))
    return data['result']


def token(node):
    if not node.get('lsp'):
        return None
    text = resolve(node['operator_token_file']).read_text()
    if node.get('operator_token_key'):
        prefix = node['operator_token_key'] + ':'
        for line in text.splitlines():
            if line.strip().startswith(prefix):
                return line.strip()[len(prefix):].strip()
        raise ValueError('operator token key not found')
    return text.strip()


def identity(pid):
    try:
        stat = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
        return None if stat[0] == 'Z' else stat[19]
    except FileNotFoundError:
        return None


def alive(record):
    return identity(record['pid']) == record['start_time'] and record['start_time'] is not None


def records(state):
    path = state / 'processes.json'
    return json.loads(path.read_text()) if path.exists() else []


def save(state, entries):
    temp = state / 'processes.tmp'
    temp.write_text(json.dumps(entries, indent=2) + '\n')
    temp.replace(state / 'processes.json')


def stop_nodes(state):
    entries = records(state)
    for record in entries:
        if alive(record):
            os.kill(record['pid'], signal.SIGTERM)
    deadline = time.monotonic() + 30
    while any(alive(r) for r in entries) and time.monotonic() < deadline:
        time.sleep(.2)
    if any(alive(r) for r in entries):
        raise RuntimeError('Some nodes did not stop after 30s; PID records retained. Inspect logs.')
    save(state, [])


def wait_rpc(url, method, auth=None, child=None, timeout=120):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        if child is not None and child.poll() is not None:
            raise RuntimeError('FNN exited during startup; inspect scripts/logs.sh <node-id>')
        try:
            return rpc(url, method, token=auth)
        except Exception as exc:
            last = exc
            time.sleep(1)
    raise RuntimeError(f'{method} readiness timed out: {last}')


def preflight(cfg):
    for node in cfg['nodes']:
        binary = resolve(node['binary'])
        if not binary.is_file() or not os.access(binary, os.X_OK):
            raise RuntimeError(f'Node {node["id"]} binary is not executable: {binary}')
        if node.get('lsp'):
            if not resolve(node['biscuit_private_key']).is_file():
                raise RuntimeError('Biscuit private key file missing')
            if not token(node):
                raise RuntimeError('Operator token is empty')
        for port in (21713 + node['id'], 8343 + node['id']):
            with socket.socket() as sock:
                sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                sock.bind(('127.0.0.1', port))


def start(cfg, state, env):
    if any(alive(r) for r in records(state)):
        raise RuntimeError('Nodes already running; stop them before starting again')
    preflight(cfg)
    # Fixed offckb ports: reject another CKB instead of attaching to the wrong chain.
    chain_pid = state / 'ckb-owned'
    if not chain_pid.exists():
        for port in (8114, 28114):
            with socket.socket() as sock:
                sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                sock.bind(('127.0.0.1', port))
        off(cfg, env, 'node', '--daemon')
        chain_pid.touch()
    else:
        try:
            rpc('http://127.0.0.1:8114', 'get_tip_header')
        except Exception:
            off(cfg, env, 'node', '--daemon')
    wait_rpc('http://127.0.0.1:8114', 'get_tip_header')
    base = state / 'data/offckb-nodejs/devnet/fiber/nodes'
    # Initialize using unauthenticated ordinary FNNs; offckb's probes lack Bearer auth.
    try:
        off(cfg, env, 'fiber', 'start', '--nodes', str(len(cfg['nodes'])), '--binary-path', str(resolve(cfg['nodes'][0]['binary'])), '--daemon')
    finally:
        off(cfg, env, 'fiber', 'stop')
    children = []
    entries = []
    save(state, entries)
    try:
        for node in cfg['nodes']:
            node_id = node['id']
            directory = base / str(node_id)
            args = [str(resolve(node['binary'])), '-d', str(directory)]
            args += ['-s', 'fiber,rpc,ckb,lsp' if node.get('lsp') else 'fiber,rpc,ckb']
            if node.get('lsp'):
                args += ['--rpc-enabled-modules', 'channel,payment,graph,info,invoice,lsp,peer,pubsub,watchtower', '--rpc-biscuit-public-key', node['biscuit_public_key'], '--rpc-biscuit-private-key-path', str(resolve(node['biscuit_private_key'])), '--fiber-auto-accept-channel-ckb-funding-amount', str(node.get('auto_accept_shannons', 50000000000))]
            args += node.get('args', [])
            child_env = env.copy()
            child_env.update({'FIBER_SECRET_KEY_PASSWORD': (directory / 'password').read_text().strip(), 'RUST_LOG': cfg.get('rust_log', 'info'), 'NO_COLOR': '1'})
            log = state / f'node-{node_id}.log'
            with log.open('ab') as output:
                child = subprocess.Popen(args, env=child_env, stdout=output, stderr=output, start_new_session=True)
            children.append(child)
            entries.append({'id': node_id, 'pid': child.pid, 'start_time': identity(child.pid), 'binary': args[0]})
            save(state, entries)
            info = wait_rpc(f'http://127.0.0.1:{21713 + node_id}', 'node_info', token(node), child)
            print(f'Node {node_id} ready: {info.get("version")} RPC :{21713 + node_id}', flush=True)
        first = cfg['nodes'][0]
        url = f'http://127.0.0.1:{21713 + first["id"]}'
        for node in cfg['nodes'][1:]:
            info = rpc(f'http://127.0.0.1:{21713 + node["id"]}', 'node_info', token=token(node))
            addresses = [a for a in info.get('addresses', []) if '/p2p/' in a]
            if not addresses:
                raise RuntimeError(f'Node {node["id"]} has no advertised P2P address')
            rpc(url, 'connect_peer', [{'address': addresses[0], 'save': True}], token(first))
        deadline = time.monotonic() + 30
        while len(rpc(url, 'list_peers', token=token(first)).get('peers', [])) < len(cfg['nodes']) - 1:
            if time.monotonic() >= deadline:
                raise RuntimeError('Peer handshake timed out')
            time.sleep(1)
        print('Fiber + LSP started. Channels and tenants are created separately.')
    except BaseException:
        stop_nodes(state)
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['start', 'stop', 'status', 'logs', 'rpc', 'doctor'])
    parser.add_argument('extra', nargs='*')
    parser.add_argument('--profile', default='devnet')
    parser.add_argument('--ckb', action='store_true', help='also stop the isolated CKB daemon')
    parser.add_argument('-f', '--follow', action='store_true')
    args = parser.parse_args()
    cfg = load(args.profile)
    state = resolve(cfg.get('state_dir', '.state/' + args.profile))
    state.mkdir(parents=True, exist_ok=True)
    env = environment(state)
    with (state / 'lab.lock').open('w') as lock:
        if args.command in ('start', 'stop'):
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        if args.command == 'start':
            start(cfg, state, env)
        elif args.command == 'stop':
            stop_nodes(state)
            if args.ckb:
                off(cfg, env, 'node', 'stop')
                (state / 'ckb-owned').unlink(missing_ok=True)
        elif args.command == 'doctor':
            preflight(cfg)
            result = subprocess.run(['node', str(resolve(cfg['offckb_entry'])), 'fiber', 'start', '--help'], env=env, capture_output=True, text=True, check=True)
            if '--binary-path' not in result.stdout or '--nodes' not in result.stdout:
                raise RuntimeError('offckb build lacks Fiber support')
            print('Binaries, LSP fixtures, ports and offckb Fiber CLI checked.')
        elif args.command == 'status':
            try:
                tip = rpc('http://127.0.0.1:8114', 'get_tip_header')
                print('CKB: running, tip ' + tip['number'])
            except Exception as exc:
                print('CKB: unavailable (' + str(exc) + ')')
            for node in cfg['nodes']:
                try:
                    info = rpc(f'http://127.0.0.1:{21713 + node["id"]}', 'node_info', token=token(node))
                    print(f'Node {node["id"]}: running, version={info.get("version")}, peers={info.get("peers_count")}, channels={info.get("channel_count")}')
                except Exception as exc:
                    print(f'Node {node["id"]}: unavailable ({exc})')
        elif args.command == 'logs':
            node_id = int(args.extra[0]) if args.extra else 2
            subprocess.run(['tail', '-n', '100', *(['-f'] if args.follow else []), str(state / f'node-{node_id}.log')], check=True)
        elif args.command == 'rpc':
            if len(args.extra) not in (2, 3):
                parser.error('rpc requires NODE_ID METHOD [JSON_PARAMS]')
            node = next(n for n in cfg['nodes'] if n['id'] == int(args.extra[0]))
            print(json.dumps(rpc(f'http://127.0.0.1:{21713 + node["id"]}', args.extra[1], json.loads(args.extra[2]) if len(args.extra) == 3 else [], token(node)), indent=2))


if __name__ == '__main__':
    try:
        main()
    except (Exception, KeyboardInterrupt) as exc:
        print(f'fiber-lab: {exc}', file=sys.stderr)
        sys.exit(1)
