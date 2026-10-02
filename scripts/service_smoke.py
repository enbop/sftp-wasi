"""Isolated Fungi service fixture for smoke.py; never touches the default daemon."""
import contextlib
import json
from pathlib import Path
import re
import shutil
import socket
import subprocess
import time


class FungiService:
    def __init__(self, fungi, temp, recipe, wasm, port, passed):
        self.fungi, self.temp, self.port, self.passed = fungi, temp, port, passed
        self.home = temp / 'fungi-home'
        self.name = 'sftp-recipe-test'
        self.proc = None
        self.service_id = None
        self.manifest = temp / 'sftp-wasip2.fungi.md'
        text = recipe.read_text()
        sources = re.findall(r'^    file: (.+)$', text, flags=re.MULTILINE)
        assert len(sources) == 1, 'expected one local recipe source'
        source = Path(sources[0].strip().strip('\"\''))
        assert not source.is_absolute() and '..' not in source.parts, 'expected a relative bundle source'
        staged = temp / source
        staged.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(wasm, staged)
        for old, new in [('SFTP_PORT: "2222"', f'SFTP_PORT: "{port}"'),
                         ('port: 2222', f'port: {port}')]:
            assert text.count(old) == 1, f'recipe changed: expected one {old}'
            text = text.replace(old, new)
        self.manifest.write_text(text)

    def cli(self, *args, check=True):
        result = subprocess.run([self.fungi, '-f', str(self.home), *map(str, args)],
                                text=True, capture_output=True, timeout=180)
        if check and result.returncode:
            raise RuntimeError(f'fungi {args}: {result.stdout}\n{result.stderr}')
        return result

    def inspect(self):
        return json.loads(self.cli('service', 'inspect', self.name, '--verbose').stdout)

    def persisted_id(self):
        # inspect.id is the runtime ID, not the appdata identity in this Fungi
        # revision. Read (never modify) the one test-owned persisted record.
        records = list((self.home / 'services').glob('*/state.json'))
        assert len(records) == 1, records
        return json.loads(records[0].read_text())['local_service_id']

    def __enter__(self):
        self.cli('init')
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            rpc = sock.getsockname()[1]
        (self.home / 'config.toml').write_text(
            f'version = 3\n[rpc]\nlisten_address = "127.0.0.1:{rpc}"\n'
            '[network]\nlisten_tcp_port = 0\nlisten_udp_port = 0\n'
            'relay_enabled = false\nuse_community_relays = false\n'
            '[runtime]\ndisable_wasmtime = false\n')
        try:
            self.start_daemon()
        except BaseException:
            self.stop_daemon()
            raise
        return self

    def start_daemon(self):
        with open(self.temp / 'daemon.log', 'a') as log:
            self.proc = subprocess.Popen(
                [self.fungi, '-f', str(self.home), 'daemon', '--exit-on-stdin-close'],
                stdin=subprocess.PIPE, stdout=log, stderr=log)
        for _ in range(150):
            if self.proc.poll() is not None:
                raise RuntimeError((self.temp / 'daemon.log').read_text()[-6000:])
            if self.cli('info', 'version', check=False).returncode == 0:
                self.cli('info', 'runtime')
                return
            time.sleep(.2)
        raise TimeoutError('Fungi daemon readiness')

    def stop_daemon(self):
        if self.proc is not None:
            if self.proc.stdin and not self.proc.stdin.closed:
                self.proc.stdin.close()
            try:
                self.proc.wait(timeout=20)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=10)
            self.proc = None

    def ready(self):
        for _ in range(600):
            if self.proc.poll() is not None:
                raise RuntimeError('Fungi daemon exited')
            try:
                with socket.create_connection(('127.0.0.1', self.port), timeout=.2) as sock:
                    sock.settimeout(1)
                    if sock.recv(128).startswith(b'SSH-2.0-'):
                        return
            except OSError:
                pass
            info = self.inspect()
            if info['phase'] in ('exited', 'failed'):
                raise RuntimeError(f'SFTP service exited before readiness: {info}')
            time.sleep(.1)
        raise TimeoutError('SFTP service readiness')

    def stopped(self):
        for _ in range(100):
            try:
                with socket.create_connection(('127.0.0.1', self.port), timeout=.1):
                    pass
            except OSError:
                assert self.inspect()['phase'] != 'running'
                return
            time.sleep(.1)
        raise AssertionError('SFTP listener still open after service stop')

    @contextlib.contextmanager
    def running(self):
        try:
            if self.service_id is None:
                self.cli('service', 'apply', self.name, self.manifest, '--dry-run')
                assert self.cli('service', 'inspect', self.name, check=False).returncode != 0
                self.cli('service', 'apply', self.name, self.manifest, '--yes', '--start')
                self.ready()
                info = self.inspect()
                assert info['phase'] == 'running', info
                self.service_id = self.persisted_id()
                self.data = self.home / 'appdata/services' / self.service_id / 'files'
                assert self.data.is_dir(), self.data
                self.passed('Fungi recipe dry-run has no service; apply --start creates running Wasmtime service')
                self.cli('service', 'apply', self.name, self.manifest, '--yes', '--start')
                self.ready()
                assert self.persisted_id() == self.service_id
                self.passed('Fungi repeated apply --start preserves running service and local service ID')
            else:
                self.cli('service', 'start', self.name)
                self.ready()
                assert self.persisted_id() == self.service_id
            connection = self.cli('service', 'connect', self.name, 'sftp').stdout
            assert str(self.port) in connection, connection
            self.cli('service', 'logs', self.name, '--tail', '30')
            self.passed('Fungi inspect, bounded logs and service connect endpoint verified')
            yield self.data
        except BaseException:
            print(self.cli('service', 'inspect', self.name, check=False).stdout, flush=True)
            print(self.cli('service', 'logs', self.name, '--tail', '50', check=False).stdout, flush=True)
            print((self.temp / 'daemon.log').read_text()[-6000:], flush=True)
            raise
        finally:
            if self.service_id is not None:
                self.cli('service', 'stop', self.name)
                self.stopped()
                self.passed('Fungi service stop closes SFTP listener')

    def restart_daemon(self):
        key = self.data.parent / 'host_ed25519'
        before = key.read_bytes()
        self.stop_daemon()
        self.start_daemon()
        self.ready()
        assert self.persisted_id() == self.service_id
        assert self.inspect()['phase'] == 'running'
        assert key.read_bytes() == before
        self.passed('Fungi daemon restart restores running service, same ID and generated host key')

    def __exit__(self, exc_type, exc, tb):
        try:
            if self.proc is not None and self.proc.poll() is None:
                found = self.cli('service', 'inspect', self.name, check=False)
                if found.returncode == 0:
                    self.cli('service', 'remove', self.name)
                    assert self.cli('service', 'inspect', self.name, check=False).returncode != 0
                    self.passed('Fungi service remove removes managed service')
        finally:
            self.stop_daemon()
