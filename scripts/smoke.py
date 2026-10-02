#!/usr/bin/env python3
"""Exercise a WASIp2 SFTP server using real OpenSSH clients and optional SSHFS."""
import argparse
import contextlib
import hashlib
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import tempfile
import time


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--fungi', default=shutil.which('fungi'))
    parser.add_argument('--wasm', type=Path, required=True)
    parser.add_argument('--recipe', type=Path,
                        help='apply this recipe through an isolated Fungi daemon instead of fungi run')
    parser.add_argument('--skip-mount', action='store_true')
    args = parser.parse_args()
    if not args.fungi:
        parser.error('pass --fungi /path/to/fungi')
    wasm = args.wasm.resolve()
    passes = []
    def passed(label):
        passes.append(label)
        print('PASS', label, flush=True)

    with tempfile.TemporaryDirectory(prefix='sftp-wasip2-smoke-') as temp_name, contextlib.ExitStack() as cleanup:
        temp = Path(temp_name)
        data, state, mount = (temp / name for name in ('data', 'state', 'mount'))
        for path in (data, state, mount):
            path.mkdir()
        askpass = temp / 'askpass'
        askpass.write_text('#!/bin/sh\nprintf "demo\\n"\n')
        askpass.chmod(0o700)
        env = os.environ.copy()
        env.update(SSH_ASKPASS=str(askpass), SSH_ASKPASS_REQUIRE='force', DISPLAY='sftp-test')
        subprocess.run(['ssh-keygen', '-q', '-t', 'ed25519', '-N', '', '-f', str(state / 'host_ed25519')], check=True)
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        service = None
        if args.recipe:
            from service_smoke import FungiService
            service = cleanup.enter_context(FungiService(args.fungi, temp, args.recipe, wasm, port, passed))
        opts = ['-oStrictHostKeyChecking=accept-new', '-oUserKnownHostsFile=' + str(temp / 'known_hosts'),
                '-oPreferredAuthentications=password', '-oPubkeyAuthentication=no', '-oBatchMode=no',
                '-oConnectTimeout=5']
        remote = f'demo@127.0.0.1'
        def run(command, input=None):
            proc = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                    text=True, env=env, start_new_session=True)
            try:
                out, err = proc.communicate(input, timeout=45)
            except BaseException:
                os.killpg(proc.pid, signal.SIGKILL)
                proc.wait()
                raise
            if proc.returncode:
                raise AssertionError(f'{command[0]} exited {proc.returncode}\n{out}\n{err}')
            return out
        def scp(*arguments):
            return run(['scp', *opts, '-P', str(port), *map(str, arguments)])
        def sftp(commands):
            return run(['sftp', *opts, '-P', str(port), '-b', '-', remote], commands + '\nbye\n')

        @contextlib.contextmanager
        def server():
            nonlocal data
            if service:
                with service.running() as service_data:
                    data = service_data
                    yield
                return
            command = [args.fungi, 'run', '-Scli', '-Stcp', '-Sinherit-network',
                       '--dir', f'{data}::data', '--dir', f'{state}::state',
                       '--env', 'SFTP_BIND_HOST=127.0.0.1', '--env', f'SFTP_PORT={port}',
                       '--env', 'SFTP_HOST_KEY=state/host_ed25519', str(wasm)]
            with open(temp / 'server.log', 'a') as log:
                proc = subprocess.Popen(command, stdout=log, stderr=log)
                try:
                    for _ in range(600):
                        if proc.poll() is not None:
                            raise RuntimeError('component exited before readiness')
                        try:
                            with socket.create_connection(('127.0.0.1', port), timeout=.1):
                                break
                        except OSError:
                            time.sleep(.1)
                    else:
                        raise TimeoutError('component readiness')
                    yield
                except BaseException:
                    print((temp / 'server.log').read_text()[-6000:], flush=True)
                    raise
                finally:
                    if proc.poll() is None:
                        proc.terminate()
                        try:
                            proc.wait(timeout=10)
                        except subprocess.TimeoutExpired:
                            proc.kill()
                            proc.wait(timeout=10)

        with server():
            source = temp / 'source.bin'
            source.write_bytes(bytes(range(256)) * 8192)
            scp(source, f'{remote}:/blob.bin')
            downloaded = temp / 'downloaded.bin'
            scp(f'{remote}:/blob.bin', downloaded)
            assert digest(source) == digest(downloaded) == digest(data / 'blob.bin')
            passed('scp 2MiB binary upload/download, SHA256 and exit code 0')
            source.write_bytes(b'short replacement')
            scp(source, f'{remote}:/blob.bin')
            assert (data / 'blob.bin').read_bytes() == source.read_bytes()
            passed('scp overwrite truncates previous content')
            empty = temp / 'empty'
            empty.touch()
            scp(empty, f'{remote}:/empty')
            assert (data / 'empty').stat().st_size == 0
            a, b = temp / 'a.txt', temp / 'b.txt'
            a.write_bytes(b'a'); b.write_bytes(b'b')
            sftp('mkdir /multiple')
            scp(a, b, f'{remote}:/multiple/')
            assert (data / 'multiple/a.txt').read_bytes() == b'a'
            assert (data / 'multiple/b.txt').read_bytes() == b'b'
            passed('scp empty file and multiple files')
            tree = temp / 'tree'
            (tree / '子目录').mkdir(parents=True)
            (tree / '子目录/空 格.txt').write_text('你好 WASIp2\n')
            scp('-r', tree, f'{remote}:/tree')
            copied = temp / 'copied'
            scp('-r', f'{remote}:/tree', copied)
            assert digest(tree / '子目录/空 格.txt') == digest(copied / '子目录/空 格.txt')
            passed('scp -r upload/download with Unicode and spaces')
            stamp = 1700000000
            os.utime(source, (stamp, stamp))
            scp('-p', source, f'{remote}:/preserved.bin')
            assert int((data / 'preserved.bin').stat().st_mtime) == stamp
            scp('-p', f'{remote}:/preserved.bin', downloaded)
            assert int(downloaded.stat().st_mtime) == stamp
            assert downloaded.stat().st_mode & 0o200, 'downloaded file unexpectedly read-only'
            passed('scp -p modification time in both directions (POSIX modes excluded on WASIp2)')
            target = temp / 'sftp-download'
            output = sftp(f'pwd\nls /\nmkdir /ops\ncd /ops\ncd ..\nput "{a}" /ops/upload.txt\nrename /ops/upload.txt /ops/renamed.txt\nget /ops/renamed.txt "{target}"\nrm /ops/renamed.txt\nrmdir /ops')
            assert target.read_bytes() == b'a'
            assert not (data / 'ops').exists()
            passed('OpenSSH sftp navigation, mkdir, put/get, rename, rm, rmdir')

            if args.skip_mount:
                print('SKIP SSHFS explicitly requested', flush=True)
            else:
                if not shutil.which('sshfs') or not Path('/dev/fuse').exists():
                    raise RuntimeError('SSHFS validation requires sshfs and /dev/fuse; use --skip-mount to explicitly omit it')
                with open(temp / 'sshfs.log', 'w') as mount_log:
                    mount_proc = subprocess.Popen(['sshfs', '-f', *opts, '-p', str(port), f'{remote}:/', str(mount)],
                                                  stdout=mount_log, stderr=mount_log, env=env)
                    try:
                        for _ in range(100):
                            if os.path.ismount(mount):
                                break
                            if mount_proc.poll() is not None:
                                raise RuntimeError((temp / 'sshfs.log').read_text())
                            time.sleep(.1)
                        assert os.path.ismount(mount), 'mount readiness'
                        assert (mount / 'blob.bin').read_bytes() == b'short replacement'
                        folder = mount / 'mounted-dir'; folder.mkdir()
                        file = folder / '文件 with spaces.txt'; file.write_bytes(b'original')
                        with file.open('ab') as opened:
                            opened.write(b' appended')
                        assert file.read_bytes() == b'original appended'
                        with file.open('r+b') as opened:
                            opened.seek(3); opened.write(b'XX'); opened.truncate(8)
                        assert file.read_bytes() == b'oriXXnal'
                        passed('SSHFS mounted read/write, append, random writes and truncate')
                        with file.open('rb') as opened:
                            moved = folder / 'moved'; file.rename(moved)
                            file.write_bytes(b'replacement')
                            assert opened.read() == b'oriXXnal'
                        staged = folder / '.editor-save'; staged.write_bytes(b'atomic replacement')
                        os.replace(staged, file)
                        assert file.read_bytes() == b'atomic replacement'
                        file.unlink(); moved.unlink(); folder.rmdir()
                        passed('SSHFS open handle across rename, editor-style replacement and deletion')
                    finally:
                        if os.path.ismount(mount):
                            subprocess.run(['fusermount3', '-u', str(mount)], check=True, timeout=10)
                        if mount_proc.poll() is None:
                            mount_proc.terminate()
                        mount_proc.wait(timeout=10)
                # Re-mount the same data and check it is still usable.
                remount = subprocess.Popen(['sshfs', '-f', *opts, '-p', str(port), f'{remote}:/', str(mount)],
                                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=env)
                try:
                    for _ in range(100):
                        if os.path.ismount(mount) or remount.poll() is not None:
                            break
                        time.sleep(.1)
                    assert os.path.ismount(mount)
                    assert (mount / 'blob.bin').read_bytes() == b'short replacement'
                finally:
                    if os.path.ismount(mount):
                        subprocess.run(['fusermount3', '-u', str(mount)], check=True, timeout=10)
                    if remount.poll() is None:
                        remount.terminate()
                    remount.wait(timeout=10)
                passed('SSHFS unmount and remount')
        with server():
            scp(f'{remote}:/blob.bin', downloaded)
            assert downloaded.read_bytes() == b'short replacement'
            passed('component restart preserves data and host key accepted by existing known_hosts')
            if service:
                service.restart_daemon()
                scp(f'{remote}:/blob.bin', downloaded)
                assert downloaded.read_bytes() == b'short replacement'
                passed('scp after daemon restart preserves data and accepts existing known_hosts')
    print(f'PASS {len(passes)} scenarios; test mounts/processes/data cleaned', flush=True)


if __name__ == '__main__':
    main()
