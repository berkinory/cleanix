#!/usr/bin/env python3
import argparse
import base64
import json
import re
import subprocess
import tempfile
from pathlib import Path

REPO = 'berkinory/cleanix'
TAP = 'berkinory/homebrew-brew'
TARGETS = ('aarch64-apple-darwin', 'x86_64-apple-darwin',
           'aarch64-unknown-linux-gnu', 'x86_64-unknown-linux-gnu')


def api(endpoint, payload=None, missing=False):
    command = ['gh', 'api', endpoint]
    if payload is not None:
        command += ['--method', 'PUT', '--input', '-']
    result = subprocess.run(command, input=json.dumps(payload) if payload else None,
                            text=True, capture_output=True, timeout=45)
    if result.returncode:
        if missing and 'HTTP 404' in result.stderr:
            return None
        raise RuntimeError(result.stderr.strip())
    return json.loads(result.stdout)


def stable(value):
    if not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', value):
        raise ValueError(f'Invalid stable version: {value}')
    return tuple(map(int, value.split('.')))


def formula(version, hashes):
    def platform(os, target):
        extra = '    depends_on macos: :big_sur\n\n' if os == 'macos' else ''
        blocks = []
        for arch, cpu in [('aarch64', 'arm'), ('x86_64', 'intel')]:
            name = f'cleanix-{arch}-{target}.tar.gz'
            blocks.append(f'''    on_{cpu} do
      url "https://github.com/{REPO}/releases/download/v#{{version}}/{name}"
      sha256 "{hashes[name]}"
    end''')
        return f'  on_{os} do\n{extra}' + '\n\n'.join(blocks) + '\n  end'
    return f'''class Cleanix < Formula
  desc "Developer cleanup tool for macOS and Linux"
  homepage "https://github.com/{REPO}"
  version "{version}"
  license "MIT"

{platform('macos', 'apple-darwin')}

{platform('linux', 'unknown-linux-gnu')}

  def install
    bin.install "cleanix"
  end

  test do
    assert_equal "cleanix #{{version}}", shell_output("#{{bin}}/cleanix --version").strip
  end
end
'''


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('version')
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument('--output', type=Path)
    action.add_argument('--publish', action='store_true')
    args = parser.parse_args()
    version = args.version.removeprefix('v')
    requested = stable(version)
    release = api(f'repos/{REPO}/releases/tags/v{version}')
    if release['draft'] or release['prerelease'] or release['tag_name'] != f'v{version}':
        raise ValueError('Homebrew requires a published stable release')
    assets = {asset['name']: asset for asset in release['assets']}
    with tempfile.TemporaryDirectory(prefix='cleanix-formula-') as directory:
        subprocess.run(['gh', 'release', 'download', f'v{version}', '--repo', REPO,
                        '--pattern', 'SHA256SUMS', '--dir', directory], check=True, timeout=60)
        hashes = {}
        for line in Path(directory, 'SHA256SUMS').read_text().splitlines():
            digest, name = line.split()
            if name in hashes or not re.fullmatch(r'[0-9a-f]{64}', digest):
                raise ValueError('Invalid checksum manifest')
            hashes[name] = digest
    for target in TARGETS:
        name = f'cleanix-{target}.tar.gz'
        if name not in hashes or assets.get(name, {}).get('digest') != f'sha256:{hashes[name]}':
            raise ValueError(f'Release asset digest does not match manifest: {name}')
    content = formula(version, hashes)
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(content)
        print(f'Wrote {args.output}')
        return
    endpoint = f'repos/{TAP}/contents/Formula/cleanix.rb'
    existing = api(endpoint, missing=True)
    payload = {'message': f'Update cleanix to {version}',
               'content': base64.b64encode(content.encode()).decode()}
    if existing:
        old = base64.b64decode(existing['content']).decode()
        match = re.search(r'^  version "([^"]+)"$', old, re.MULTILINE)
        if not match:
            raise ValueError('Cannot identify existing formula version')
        if stable(match[1]) > requested or old == content:
            print(f'Keeping existing cleanix {match[1]} formula')
            return
        payload['sha'] = existing['sha']
    result = api(endpoint, payload)
    print(result['commit']['html_url'])


if __name__ == '__main__':
    main()
