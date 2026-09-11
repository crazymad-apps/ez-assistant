"""下载并核验官方 Node；urllib 使用宿主的代理配置，不改全局网络或信任设置。"""
import argparse
import hashlib
import pathlib
import urllib.request

parser = argparse.ArgumentParser()
parser.add_argument('--arch', choices=['arm64', 'x64'], required=True)
parser.add_argument('--output', type=pathlib.Path, required=True)
parser.add_argument('--mirror', action='store_true', help='仅从 npmmirror 下载归档，摘要仍以 Node 官网为准')
args = parser.parse_args()
root = args.output.resolve()
root.mkdir(parents=True, exist_ok=True)
name = f'node-v24.21.0-linux-{args.arch}.tar.xz'
base = 'https://nodejs.org/dist/v24.21.0/'
manifest = urllib.request.urlopen(base+'SHASUMS256.txt', timeout=30).read()
expected = next(line.split()[0] for line in manifest.decode().splitlines() if line.split()[-1] == name)
archive = root/'node.tar.xz'
def digest(path):
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()
if not archive.exists() or digest(archive) != expected:
    temporary = root/'node.tar.xz.download'
    archive_base = 'https://registry.npmmirror.com/-/binary/node/v24.21.0/' if args.mirror else base
    with urllib.request.urlopen(archive_base+name, timeout=60) as response, temporary.open('wb') as destination:
        while chunk := response.read(1024*1024):
            destination.write(chunk)
    assert digest(temporary) == expected, '官方 Node 包摘要不匹配，停止构建'
    temporary.replace(archive)
(root/'node.sha256').write_text(expected+'  node.tar.xz\n')
(root/'SHASUMS256.txt').write_bytes(manifest)
print(f'Node 24.21.0 linux-{args.arch} verified: {expected}\nBuild context: {root}')
