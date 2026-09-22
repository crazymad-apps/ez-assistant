#!/usr/bin/env bash
set -euo pipefail

# 只上传已装配的制品，远端 Dockerfile 不执行依赖安装或编译。
# 首次部署准备 center.env、数据库和持久目录后，再单独执行 compose up。
bundle="${1:?Usage: deploy.sh BUNDLE_DIRECTORY IMAGE_TAG}"
tag="${2:?Specify a unique image tag}"
[[ "$tag" =~ ^[a-zA-Z0-9_][a-zA-Z0-9_.-]*$ ]] || exit 2
target="${CENTER_SSH_TARGET:-root@172.16.20.4}"
remote_root="/home/1panel/apps/ez-enterprise-center/ez-enterprise-center"
image="ez-enterprise-center:${tag}"
script_dir="$(cd "$(dirname "$0")" && pwd)"
for required in Dockerfile package.json node_modules dist public; do
  test -e "$bundle/$required"
done
ssh -o BatchMode=yes "$target" "mkdir -p '$remote_root/releases'; mkdir '$remote_root/releases/$tag'"
tar_options=()
if [[ "$(uname -s)" == Darwin ]]; then tar_options+=(--no-xattrs); fi
COPYFILE_DISABLE=1 tar "${tar_options[@]}" -C "$bundle" -czf - Dockerfile .dockerignore package.json package-lock.json node_modules dist public \
  | ssh -o BatchMode=yes "$target" "tar -xzf - -C '$remote_root/releases/$tag'"
scp -o BatchMode=yes "$script_dir/compose.yaml" "$target:$remote_root/releases/$tag/compose.yaml"
ssh -o BatchMode=yes "$target" "docker build -t '$image' '$remote_root/releases/$tag'"
printf 'Image: %s\nRelease directory: %s/releases/%s\n' "$image" "$remote_root" "$tag"
