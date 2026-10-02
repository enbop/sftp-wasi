#!/usr/bin/env python3
"""Build a local, checksummed experiment bundle from committed and pinned sources."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import tomllib


ROOT = Path(__file__).resolve().parents[1]


def output(*args, cwd=ROOT):
    return subprocess.check_output(args, cwd=cwd)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def verify_dependencies(offline=False):
    manifest = tomllib.loads((ROOT / 'Cargo.toml').read_text())
    command = ['cargo', 'metadata', '--locked', '--format-version', '1', '--filter-platform', 'wasm32-wasip2']
    if offline:
        command.append('--offline')
    metadata = json.loads(output(*command))
    dependencies = []
    for name in ('russh', 'russh-sftp'):
        dep = manifest['dependencies'][name]
        revision = dep['rev']
        if len(revision) != 40 or any(c not in '0123456789abcdef' for c in revision):
            raise RuntimeError(f'{name}: expected a full Git commit revision')
        source = f"git+{dep['git']}?rev={revision}#{revision}"
        matches = [p for p in metadata['packages'] if p['name'] == name and p['source'] == source]
        if len(matches) != 1:
            raise RuntimeError(f'{name}: Cargo resolution does not match pinned Git revision')
        dependencies.append({'name': name, 'repository': dep['git'],
                             'revision': revision, 'version': matches[0]['version']})
    return {'dependencies': dependencies}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true', help='verify the pinned Git dependencies in Cargo resolution')
    parser.add_argument('--offline', action='store_true')
    parser.add_argument('--output', type=Path, help='new output directory (must not exist)')
    args = parser.parse_args()
    lock = verify_dependencies(args.offline)
    if args.check:
        print('PASS Cargo resolves the exact pinned Git revisions')
        return
    if output('git', 'status', '--porcelain'):
        raise RuntimeError('Commit the experiment sources before packaging; working tree must be clean')
    revision = output('git', 'rev-parse', 'HEAD').decode().strip()
    destination = args.output or ROOT / 'dist' / f'sftp-wasi-{revision[:12]}'
    destination = destination.resolve()
    archive = destination.with_suffix('.tar.gz')
    if destination.exists() or archive.exists():
        raise RuntimeError(f'Refusing to overwrite existing output: {destination}')
    command = ['cargo', 'build', '--locked', '--release', '--target', 'wasm32-wasip2']
    if args.offline:
        command.append('--offline')
    subprocess.run(command, cwd=ROOT, check=True)
    metadata = json.loads(output('cargo', 'metadata', '--no-deps', '--format-version', '1'))
    wasm = Path(metadata['target_directory']) / 'wasm32-wasip2/release/sftp-wasi.wasm'
    destination.mkdir(parents=True)
    shutil.copyfile(wasm, destination / 'sftp-wasi.wasm')
    source_path = './target/wasm32-wasip2/release/sftp-wasi.wasm'
    recipe = (ROOT / 'sftp-wasi.fungi.md').read_text()
    assert recipe.count(source_path) == 1
    recipe = recipe.replace(source_path, './sftp-wasi.wasm')
    recipe = recipe.split('\n# SFTP WASI', 1)[0]
    recipe += '\n# SFTP WASI\n\nPrebuilt local recipe. See README.md.\n'
    (destination / 'sftp-wasi.fungi.md').write_text(recipe)
    (destination / 'README.md').write_text(
        '# SFTP WASIp2 local bundle\n\n'
        'Requires Fungi with merged PR #79 and its WASI environment fix; '
        'the v0.7.1 release is too old. Tested core commit and results are '
        'recorded in VALIDATION.md. Start a compatible Fungi daemon first.\n\n'
        '```sh\nsha256sum -c SHA256SUMS\n'
        'fungi service apply sftp-demo ./sftp-wasi.fungi.md --dry-run\n'
        'fungi service apply sftp-demo ./sftp-wasi.fungi.md --start\n'
        'fungi service connect sftp-demo sftp\n```\n\n'
        'Default: 127.0.0.1:2222, username/password demo/demo. Modern scp, '
        'SFTP and SSHFS; no remote shell or legacy scp -O. Files and a '
        'generated SSH host key persist in service-owned appdata.\n\n'
        'This is an experimental local bundle, not an official catalog release. '
        'source.tar.gz contains the committed application, pinned dependency '
        'revisions, build instructions and reproducible smoke tests.\n')
    shutil.copyfile(ROOT / 'VALIDATION.md', destination / 'VALIDATION.md')
    output('git', 'archive', '--format=tar.gz', '--prefix=sftp-wasi/',
           '--output=' + str(destination / 'source.tar.gz'), 'HEAD')
    info = {
        'source_revision': revision,
        'rustc': output('rustc', '--version').decode().strip(),
        'cargo': output('cargo', '--version').decode().strip(),
        'target': 'wasm32-wasip2',
        'cargo_lock_sha256': digest(ROOT / 'Cargo.lock'),
        'dependencies': lock['dependencies'],
        'wasm_sha256': digest(destination / 'sftp-wasi.wasm'),
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
