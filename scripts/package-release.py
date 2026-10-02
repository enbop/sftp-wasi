#!/usr/bin/env python3
"""Build a local, checksummed experiment bundle from committed and pinned sources."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tarfile


ROOT = Path(__file__).resolve().parents[1]


def output(*args, cwd=ROOT):
    return subprocess.check_output(args, cwd=cwd)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def verify_dependencies():
    lock = json.loads((ROOT / 'dependencies.lock.json').read_text())
    for dep in lock['dependencies']:
        directory = (ROOT / dep['path']).resolve()
        revision = output('git', 'rev-parse', 'HEAD', cwd=directory).decode().strip()
        if revision != dep['revision']:
            raise RuntimeError(f"{dep['path']}: expected {dep['revision']}, found {revision}")
        # Include staged edits, and do not silently omit untracked source files.
        if output('git', 'ls-files', '--others', '--exclude-standard', cwd=directory):
            raise RuntimeError(f"{dep['path']}: unexpected untracked files")
        actual = output('git', '-c', 'diff.noprefix=false', 'diff', '--no-ext-diff',
                        '--no-color', '--binary', 'HEAD', cwd=directory)
        expected = b''
        if 'patch' in dep:
            patch = ROOT / dep['patch']
            if digest(patch) != dep['patch_sha256']:
                raise RuntimeError(f"{dep['path']}: patch checksum mismatch")
            expected = patch.read_bytes()
        if actual != expected:
            raise RuntimeError(f"{dep['path']}: checkout does not match the pinned patch")
    return lock


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true', help='verify only the local dependency sources')
    parser.add_argument('--offline', action='store_true')
    parser.add_argument('--output', type=Path, help='new output directory (must not exist)')
    args = parser.parse_args()
    lock = verify_dependencies()
    if args.check:
        print('PASS dependency revisions, patch checksum and checkout contents')
        return
    if output('git', 'status', '--porcelain'):
        raise RuntimeError('Commit the experiment sources before packaging; working tree must be clean')
    revision = output('git', 'rev-parse', 'HEAD').decode().strip()
    destination = args.output or ROOT / 'dist' / f'sftp-wasip2-{revision[:12]}'
    destination = destination.resolve()
    archive = destination.with_suffix('.tar.gz')
    if destination.exists() or archive.exists():
        raise RuntimeError(f'Refusing to overwrite existing output: {destination}')
    command = ['cargo', 'build', '--locked', '--release', '--target', 'wasm32-wasip2']
    if args.offline:
        command.append('--offline')
    subprocess.run(command, cwd=ROOT, check=True)
    metadata = json.loads(output('cargo', 'metadata', '--no-deps', '--format-version', '1'))
    wasm = Path(metadata['target_directory']) / 'wasm32-wasip2/release/sftp-wasip2-experiment.wasm'
    destination.mkdir(parents=True)
    shutil.copyfile(wasm, destination / 'sftp-wasip2.wasm')
    source_path = './target/wasm32-wasip2/release/sftp-wasip2-experiment.wasm'
    recipe = (ROOT / 'sftp-wasip2.fungi.md').read_text()
    assert recipe.count(source_path) == 1
    recipe = recipe.replace(source_path, './sftp-wasip2.wasm')
    recipe = recipe.split('\n# SFTP WASIp2 experiment', 1)[0]
    recipe += '\n# SFTP WASIp2 experiment\n\nPrebuilt local recipe. See README.md.\n'
    (destination / 'sftp-wasip2.fungi.md').write_text(recipe)
    (destination / 'README.md').write_text(
        '# SFTP WASIp2 local bundle\n\n'
        'Requires Fungi with merged PR #79 and its WASI environment fix; '
        'the v0.7.1 release is too old. Tested core commit and results are '
        'recorded in VALIDATION.md. Start a compatible Fungi daemon first.\n\n'
        '```sh\nsha256sum -c SHA256SUMS\n'
        'fungi service apply sftp-demo ./sftp-wasip2.fungi.md --dry-run\n'
        'fungi service apply sftp-demo ./sftp-wasip2.fungi.md --start\n'
        'fungi service connect sftp-demo sftp\n```\n\n'
        'Default: 127.0.0.1:2222, username/password demo/demo. Modern scp, '
        'SFTP and SSHFS; no remote shell or legacy scp -O. Files and a '
        'generated SSH host key persist in service-owned appdata.\n\n'
        'This is an experimental local bundle, not an official catalog release. '
        'source.tar.gz contains the committed application, pinned dependency '
        'revisions, russh patch, build instructions and reproducible smoke tests.\n')
    shutil.copyfile(ROOT / 'VALIDATION.md', destination / 'VALIDATION.md')
    output('git', 'archive', '--format=tar.gz', '--prefix=sftp-wasip2-experiment/',
           '--output=' + str(destination / 'source.tar.gz'), 'HEAD')
    info = {
        'source_revision': revision,
        'rustc': output('rustc', '--version').decode().strip(),
        'cargo': output('cargo', '--version').decode().strip(),
        'target': 'wasm32-wasip2',
        'cargo_lock_sha256': digest(ROOT / 'Cargo.lock'),
        'dependencies': lock['dependencies'],
        'wasm_sha256': digest(destination / 'sftp-wasip2.wasm'),
    }
    (destination / 'BUILD-INFO.json').write_text(json.dumps(info, indent=2) + '\n')
    files = sorted(path for path in destination.iterdir() if path.is_file())
    (destination / 'SHA256SUMS').write_text(''.join(f'{digest(p)}  {p.name}\n' for p in files))
    with tarfile.open(archive, 'w:gz') as bundle:
        bundle.add(destination, arcname=destination.name)
    print(destination)
    print(f'{digest(archive)}  {archive.name}')


if __name__ == '__main__':
    main()
