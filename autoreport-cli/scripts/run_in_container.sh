#!/usr/bin/env bash
# Run AutoReport in a Docker image that includes init_firewall.sh.
set -euo pipefail

work_dir="${WORKSPACE_ROOT_DIR:-$(pwd)}"
image="${AUTOREPORT_CONTAINER_IMAGE:-autoreport}"
domains="${AUTOREPORT_ALLOWED_DOMAINS:-api.openai.com api.anthropic.com}"
if [[ "${1:-}" == "--work-dir" ]]; then work_dir="$2"; shift 2; fi
test "$#" -gt 0 || { echo "usage: $0 [--work-dir DIR] <autoreport args...>" >&2; exit 2; }
work_dir=$(realpath "$work_dir")
name="autoreport_$(echo "$work_dir" | tr '/.' '__' | tr -cd '[:alnum:]_-')"
cleanup() { docker rm -f "$name" >/dev/null 2>&1 || true; }; trap cleanup EXIT
docker run --name "$name" -d --cap-add=NET_ADMIN --cap-add=NET_RAW -v "$work_dir:/workspace" "$image" sleep infinity
docker exec --user root "$name" mkdir -p /etc/autoreport
printf '%s\n' $domains | docker exec --user root -i "$name" tee /etc/autoreport/allowed_domains.txt >/dev/null
docker exec --user root "$name" /usr/local/bin/init_firewall.sh
docker exec -it "$name" sh -lc 'cd /workspace && exec autoreport "$@"' -- "$@"
