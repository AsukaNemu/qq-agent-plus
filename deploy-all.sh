#!/usr/bin/env bash
set -euo pipefail
umask 077

SOURCE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="${QQ_AGENT_STACK_DIR:-/mnt/data/qq-agent}"
AGENT_PORT="3210"
SNOWLUMA_PORT="5099"
NOVNC_PORT="6081"
ONEBOT_HTTP_PORT="3000"
ONEBOT_WS_PORT="3001"
AGENT_PORT_SET=false
SNOWLUMA_PORT_SET=false
NOVNC_PORT_SET=false
ONEBOT_HTTP_PORT_SET=false
ONEBOT_WS_PORT_SET=false
SERVICE="qq-agent-linux"
SERVICE_SET=false
IMAGE="${SNOWLUMA_IMAGE:-motricseven7/snowluma:v1.14.15}"
IMAGE_SET=false
IMAGE_MIRROR_ARG=""
# 这两个是用户会按机器情况调的旋钮（显存紧张时改 SNOWLUMA_SCREEN、排查时改日志级别），
# 重跑时从 .env 回读，避免手工改动被默认值覆盖。
SNOWLUMA_SCREEN="1920x1080x24"
SNOWLUMA_LOG_LEVEL="info"
ASSUME_YES=false
CHECK_ONLY=false
ROTATE_CREDENTIALS=false
INSTALL_DOCKER=true
AGENT_TOKEN=""
ONEBOT_TOKEN=""
SNOWLUMA_PASSWORD=""
VNC_PASSWORD=""
SNOWLUMA_CURRENT_PASSWORD=""
SNOWLUMA_TOTP=""
MODEL_BASE_URL="${QQ_AGENT_MODEL_BASE_URL:-}"
MODEL_API_KEY="${QQ_AGENT_MODEL_API_KEY:-}"
MODEL_NAME="${QQ_AGENT_MODEL:-}"
ALLOW_GROUPS="${QQ_AGENT_ALLOW_GROUPS:-}"
ALLOW_PRIVATE="${QQ_AGENT_ALLOW_PRIVATE:-}"
SKIP_MODEL_CONFIG=false

usage() {
  cat <<'EOF'
Usage: bash deploy-all.sh [options]

Interactive full-stack installer for QQ Agent + SnowLuma/OneBot.

Options:
  --root-dir PATH             Stack root (default: /mnt/data/qq-agent)
  --agent-port PORT           QQ Agent console port (default: 3210)
  --snowluma-port PORT        SnowLuma WebUI port (default: 5099)
  --novnc-port PORT           QQ login/noVNC port (default: 6081)
  --onebot-http-port PORT     Local-only OneBot HTTP port (default: 3000)
  --onebot-ws-port PORT       Local-only OneBot WebSocket port (default: 3001)
  --service NAME              systemd user service (default: qq-agent-linux)
  --image IMAGE               SnowLuma image (default: tested v1.14.15)
  --image-mirror HOST         Retry the pull through a registry mirror when Docker
                              Hub is unreachable (mainland China), e.g.
                              --image-mirror docker.m.daocloud.io
                              (also: QQ_AGENT_IMAGE_MIRROR, comma-separated)
  --agent-token TOKEN         Set the QQ Agent console token
  --onebot-token TOKEN        Set the shared OneBot HTTP/WS token
  --snowluma-password VALUE   Set the initial SnowLuma WebUI password
  --snowluma-current-password VALUE
                              Current WebUI password when rotating it
  --snowluma-totp CODE        Current SnowLuma 2FA code when enabled
  --vnc-password VALUE        Set the noVNC password (8 characters recommended)
  --model-base-url URL        OpenAI-compatible Chat Completions base URL
  --model-api-key KEY         Model provider API key
  --model NAME                Model identifier
  --allow-groups IDS          Comma-separated QQ group allowlist
  --allow-private IDS         Comma-separated QQ private-chat allowlist
  --skip-model-config         Leave model settings for the management console
  --rotate-credentials        Replace stored credentials during an update
  --no-install-docker         Fail instead of installing Docker when missing
  --check-only                Inspect ownership and ports without changing anything
  -y, --yes                   Accept defaults; suitable for non-interactive use
  -h, --help                  Show this help

The installer never asks for a LAN IP. Services bind locally and the script
detects addresses to print after deployment. OneBot ports bind to 127.0.0.1.
Existing installations not owned by this installer are never adopted.
Use deploy.sh with the existing data directory and endpoints to update Agent only.
EOF
}

require_value() {
  (($# >= 2)) || { printf 'Missing value for %s\n' "$1" >&2; exit 2; }
}

while (($#)); do
  case "$1" in
    --root-dir) require_value "$@"; ROOT_DIR="$2"; shift 2 ;;
    --agent-port) require_value "$@"; AGENT_PORT="$2"; AGENT_PORT_SET=true; shift 2 ;;
    --snowluma-port) require_value "$@"; SNOWLUMA_PORT="$2"; SNOWLUMA_PORT_SET=true; shift 2 ;;
    --novnc-port) require_value "$@"; NOVNC_PORT="$2"; NOVNC_PORT_SET=true; shift 2 ;;
    --onebot-http-port) require_value "$@"; ONEBOT_HTTP_PORT="$2"; ONEBOT_HTTP_PORT_SET=true; shift 2 ;;
    --onebot-ws-port) require_value "$@"; ONEBOT_WS_PORT="$2"; ONEBOT_WS_PORT_SET=true; shift 2 ;;
    --service) require_value "$@"; SERVICE="$2"; SERVICE_SET=true; shift 2 ;;
    --image) require_value "$@"; IMAGE="$2"; IMAGE_SET=true; shift 2 ;;
    --image-mirror) require_value "$@"; IMAGE_MIRROR_ARG="$2"; shift 2 ;;
    --agent-token) require_value "$@"; AGENT_TOKEN="$2"; shift 2 ;;
    --onebot-token) require_value "$@"; ONEBOT_TOKEN="$2"; shift 2 ;;
    --snowluma-password) require_value "$@"; SNOWLUMA_PASSWORD="$2"; shift 2 ;;
    --snowluma-current-password) require_value "$@"; SNOWLUMA_CURRENT_PASSWORD="$2"; shift 2 ;;
    --snowluma-totp) require_value "$@"; SNOWLUMA_TOTP="$2"; shift 2 ;;
    --vnc-password) require_value "$@"; VNC_PASSWORD="$2"; shift 2 ;;
    --model-base-url) require_value "$@"; MODEL_BASE_URL="$2"; shift 2 ;;
    --model-api-key) require_value "$@"; MODEL_API_KEY="$2"; shift 2 ;;
    --model) require_value "$@"; MODEL_NAME="$2"; shift 2 ;;
    --allow-groups) require_value "$@"; ALLOW_GROUPS="$2"; shift 2 ;;
    --allow-private) require_value "$@"; ALLOW_PRIVATE="$2"; shift 2 ;;
    --skip-model-config) SKIP_MODEL_CONFIG=true; shift ;;
    --rotate-credentials) ROTATE_CREDENTIALS=true; shift ;;
    --no-install-docker) INSTALL_DOCKER=false; shift ;;
    --check-only) CHECK_ONLY=true; ASSUME_YES=true; shift ;;
    -y|--yes) ASSUME_YES=true; shift ;;
    -h|--help) usage; exit 0 ;;
    *) printf 'Unknown option: %s\n\n' "$1" >&2; usage >&2; exit 2 ;;
  esac
done

# 凭据的环境变量回退：命令行参数会出现在 /proc/<pid>/cmdline（对本机所有用户
# 可读），无人值守/CI 场景请优先用环境变量传凭据。命令行显式传值时优先于环境变量。
AGENT_TOKEN="${AGENT_TOKEN:-${QQ_AGENT_AGENT_TOKEN:-}}"
ONEBOT_TOKEN="${ONEBOT_TOKEN:-${QQ_AGENT_ONEBOT_TOKEN:-}}"
SNOWLUMA_PASSWORD="${SNOWLUMA_PASSWORD:-${QQ_AGENT_SNOWLUMA_PASSWORD:-}}"
SNOWLUMA_TOTP="${SNOWLUMA_TOTP:-${QQ_AGENT_SNOWLUMA_TOTP:-}}"
VNC_PASSWORD="${VNC_PASSWORD:-${QQ_AGENT_VNC_PASSWORD:-}}"

die() {
  printf 'Error: %s\n' "$*" >&2
  exit 1
}

# 全新安装中途失败时清掉本次写下的 .env / compose 与空目录：否则重跑会被
# check_local_ownership 判成"不完整的受管安装"而拒绝 —— 而拉镜像失败的提示恰恰让操作者重跑。
# 容器可能已经写过东西，所以目录只用 rmdir：非空会失败，正好保留现场。
FRESH_STACK_CLEANUP=false
cleanup_fresh_stack() {
  [[ "$FRESH_STACK_CLEANUP" == true ]] || return 0
  printf '安装未完成：已清理本次写入的栈文件与空目录，可以直接重跑 deploy-all.sh。\n' >&2
  rm -f "$ENV_FILE" "$ENV_FILE.tmp" "$COMPOSE_FILE" "$COMPOSE_FILE.tmp"
  rmdir "$SNOWLUMA_DATA_DIR/config" "$SNOWLUMA_DIR/client-config" "$SNOWLUMA_DIR/client-data" \
    "$SNOWLUMA_DATA_DIR" "$SNOWLUMA_DIR" "$AGENT_DATA_DIR" 2>/dev/null || true
}

step() {
  printf '\n==> %s\n' "$*"
}

prompt_value() {
  local label="$1" default="$2" answer=""
  if [[ "$ASSUME_YES" == true ]]; then
    printf '%s' "$default"
    return
  fi
  [[ -r /dev/tty ]] || die 'Interactive input is unavailable; use --yes and explicit options'
  printf '%s [%s]: ' "$label" "$default" >/dev/tty
  IFS= read -r answer </dev/tty || true
  printf '%s' "${answer:-$default}"
}

confirm() {
  local label="$1" default_yes="${2:-false}" answer=""
  if [[ "$ASSUME_YES" == true ]]; then
    [[ "$default_yes" == true ]]
    return
  fi
  local suffix='[y/N]'
  [[ "$default_yes" == true ]] && suffix='[Y/n]'
  printf '%s %s ' "$label" "$suffix" >/dev/tty
  IFS= read -r answer </dev/tty || true
  if [[ -z "$answer" ]]; then
    [[ "$default_yes" == true ]]
  else
    [[ "$answer" =~ ^([yY]|yes|YES|是)$ ]]
  fi
}

prompt_secret() {
  local label="$1" answer=""
  printf '%s: ' "$label" >/dev/tty
  IFS= read -r -s answer </dev/tty || true
  printf '\n' >/dev/tty
  printf '%s' "$answer"
}

random_hex() {
  od -An -N"$1" -tx1 /dev/urandom | tr -d ' \n'
}

env_value() {
  local file="$1" key="$2"
  [[ -f "$file" ]] || return 0
  sed -n "s/^${key}=//p" "$file" | tail -n 1
}

validate_port() {
  [[ "$2" =~ ^[0-9]+$ ]] && ((10#$2 >= 1 && 10#$2 <= 65535)) \
    || die "$1 must be an integer from 1 to 65535"
}

validate_secret() {
  local label="$1" value="$2" minimum="$3"
  ((${#value} >= minimum)) || die "$label must contain at least $minimum characters"
  [[ "$value" =~ ^[A-Za-z0-9._!@%+=:-]+$ ]] \
    || die "$label may only contain letters, numbers, and ._!@%+=:-"
}

run_root() {
  if ((EUID == 0)); then
    "$@"
  elif command -v sudo >/dev/null 2>&1; then
    sudo "$@"
  else
    die "Root permission is required for: $*"
  fi
}

docker_ready() {
  command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1
}

sudo_docker_ready() {
  ((EUID != 0)) && command -v sudo >/dev/null 2>&1 \
    && command -v docker >/dev/null 2>&1 && sudo docker info >/dev/null 2>&1
}

install_docker() {
  [[ "$INSTALL_DOCKER" == true ]] || die 'Docker is unavailable and automatic installation is disabled'
  confirm 'Docker is missing. Install Docker and Compose now?' true \
    || die 'Docker is required by SnowLuma'

  step 'Installing Docker'
  if command -v apt-get >/dev/null 2>&1; then
    run_root apt-get update
    if ! run_root apt-get install -y docker.io docker-compose-v2; then
      run_root apt-get install -y docker.io docker-compose-plugin
    fi
  elif command -v dnf >/dev/null 2>&1; then
    run_root dnf install -y docker docker-compose-plugin \
      || run_root dnf install -y moby-engine docker-compose-plugin
  elif command -v pacman >/dev/null 2>&1; then
    run_root pacman -Sy --noconfirm docker docker-compose
  else
    die 'No supported package manager found; install Docker Engine and Compose manually'
  fi
  run_root systemctl enable --now docker
}

docker_call() {
  if docker_ready; then
    docker "$@"
  elif sudo_docker_ready; then
    sudo docker "$@"
  else
    return 1
  fi
}

wait_http() {
  local url="$1" attempts="${2:-60}"
  for ((i = 0; i < attempts; i++)); do
    if curl -fsS --max-time 2 "$url" >/dev/null 2>&1; then return 0; fi
    sleep 1
  done
  return 1
}

onebot_ready() {
  # Token 经 stdin 喂给 curl（-H @-），不出现在 /proc/<pid>/cmdline——
  # 对本机所有用户可读（凭据退出命令行参数原则）。
  printf 'authorization: Bearer %s\n' "$ONEBOT_TOKEN" | \
  curl -fsS --max-time 3 -H @- \
    -H 'content-type: application/json' \
    -d '{}' "http://127.0.0.1:$ONEBOT_HTTP_PORT/get_login_info" \
    | grep -Eq '"retcode"[[:space:]]*:[[:space:]]*0'
}

refuse_existing() {
  printf 'Existing or incomplete installation detected: %s\n' "$1" >&2
  die 'No deployment changes made. Use deploy.sh for the existing Agent; preserve its data directory, bind address and OneBot settings. Automatic takeover is not supported.'
}

check_local_ownership() {
  local file directory contents
  if [[ -e "$ENV_FILE" ]]; then
    for file in "$ENV_FILE" "$COMPOSE_FILE" "$APP_DIR/.deployment.json" \
      "$APP_DIR/.deployment-node" "$AGENT_DATA_DIR/config.json"; do
      [[ -f "$file" && -r "$file" && ! -L "$file" ]] \
        || refuse_existing "incomplete managed stack ($file)"
    done
    EXISTING_STACK=true
    IFS= read -r PREFLIGHT_NODE <"$APP_DIR/.deployment-node" || true
    [[ -n "$PREFLIGHT_NODE" && -x "$PREFLIGHT_NODE" ]] \
      || refuse_existing 'the recorded Node runtime is unavailable'
  else
    for file in "$APP_DIR/.deployment.json" "$APP_DIR/.deployment-node" \
      "$APP_DIR/config.json" "$ACCESS_FILE"; do
      [[ ! -e "$file" && ! -L "$file" ]] \
        || refuse_existing "$file exists without managed stack metadata"
    done
    for directory in "$AGENT_DATA_DIR" "$APP_DIR/data" "$SNOWLUMA_DIR"; do
      [[ ! -L "$directory" ]] || refuse_existing "unmanaged data symlink $directory"
      [[ -e "$directory" ]] || continue
      [[ -d "$directory" && -r "$directory" && -x "$directory" ]] \
        || refuse_existing "cannot inspect $directory"
      contents="$(find "$directory" -mindepth 1 -maxdepth 1 -print -quit)" \
        || refuse_existing "cannot inspect $directory"
      [[ -z "$contents" ]] || refuse_existing "$directory contains data without managed stack metadata"
    done
  fi
}

check_host_ownership() {
  local load_state working_dir inventory id name image own_id="" docker_ports=""
  local listeners port agent_running=false old_agent_port=""
  local docker_command=()
  command -v systemctl >/dev/null || die 'systemctl is required'
  systemctl --user show-environment >/dev/null \
    || die 'The systemd user manager is unavailable; no deployment changes made'
  if ! load_state="$(systemctl --user show "$SERVICE.service" --property=LoadState --value)"; then
    [[ "$load_state" == not-found ]] \
      || die 'Cannot inspect the selected systemd service; no deployment changes made'
  fi
  case "$load_state" in
    not-found) ;;
    loaded)
      [[ "$EXISTING_STACK" == true ]] || refuse_existing "$SERVICE.service already exists"
      working_dir="$(systemctl --user show "$SERVICE.service" --property=WorkingDirectory --value)" \
        || refuse_existing "cannot inspect $SERVICE.service"
      [[ -n "$working_dir" && "$(realpath -m -- "$working_dir")" == "$APP_DIR" ]] \
        || refuse_existing "$SERVICE.service belongs to another application directory"
      if systemctl --user is-active --quiet "$SERVICE.service"; then agent_running=true; fi
      ;;
    *) refuse_existing "cannot establish ownership of $SERVICE.service" ;;
  esac

  if [[ "$EXISTING_STACK" == true ]]; then
    "$PREFLIGHT_NODE" "$SOURCE_DIR/scripts/check-stack-update.mjs" \
      --root-dir "$ROOT_DIR" --service "$SERVICE" \
      || refuse_existing 'managed Agent configuration does not match the saved stack'
    old_agent_port="$(env_value "$ENV_FILE" AGENT_PORT)"
  fi

  if command -v docker >/dev/null 2>&1; then
    if docker_ready; then
      docker_command=(docker)
    elif command -v sudo >/dev/null 2>&1; then
      if sudo -n docker info >/dev/null 2>&1; then
        docker_command=(sudo -n docker)
      elif [[ "$CHECK_ONLY" != true && "$ASSUME_YES" != true ]] \
        && sudo docker info >/dev/null; then
        docker_command=(sudo docker)
      fi
    fi
    ((${#docker_command[@]} > 0)) \
      || refuse_existing 'Docker exists but its containers cannot be inspected (check daemon access or sudo)'
    inventory="$("${docker_command[@]}" ps -a --format '{{.ID}}|{{.Names}}|{{.Image}}')" \
      || refuse_existing 'Docker container inventory is unavailable'
    while IFS='|' read -r id name image; do
      [[ -n "$id" ]] || continue
      image="$(printf '%s' "$image" | tr '[:upper:]' '[:lower:]')"
      case "$name:$image" in
        qq-agent-snowluma:*|*[Ss][Nn][Oo][Ww][Ll][Uu][Mm][Aa]*|*[Nn][Aa][Pp][Cc][Aa][Tt]*|*[Ll][Aa][Gg][Rr][Aa][Nn][Gg][Ee]*)
          [[ "$EXISTING_STACK" == true && "$name" == qq-agent-snowluma ]] \
            || refuse_existing "external QQ gateway container $name"
          own_id="$id"
          "${docker_command[@]}" inspect "$id" \
            | "$PREFLIGHT_NODE" "$SOURCE_DIR/scripts/check-stack-update.mjs" \
                --root-dir "$ROOT_DIR" --service "$SERVICE" --input container \
            || refuse_existing "container $name does not belong to this stack"
          ;;
      esac
    done <<<"$inventory"
    if [[ "$EXISTING_STACK" == true ]]; then
      "${docker_command[@]}" compose --project-directory "$SNOWLUMA_DIR" \
        --env-file "$ENV_FILE" -f "$COMPOSE_FILE" config --format json \
        | "$PREFLIGHT_NODE" "$SOURCE_DIR/scripts/check-stack-update.mjs" \
            --root-dir "$ROOT_DIR" --service "$SERVICE" --input compose \
        || refuse_existing 'Compose configuration does not match this stack'
    fi
    if [[ -n "$own_id" ]]; then
      docker_ports="$("${docker_command[@]}" inspect --format \
        '{{if .State.Running}}{{range .NetworkSettings.Ports}}{{range .}}{{.HostPort}}{{"\n"}}{{end}}{{end}}{{end}}' "$own_id")" \
        || refuse_existing 'cannot inspect managed container ports'
    fi
  elif [[ "$EXISTING_STACK" == true ]]; then
    refuse_existing 'Docker is missing for the existing managed stack'
  fi

  command -v ss >/dev/null || die 'ss (iproute2) is required for port checks'
  listeners="$(ss -H -ltn)" || die 'Cannot inspect listening ports; no deployment changes made'
  listeners="$(printf '%s\n' "$listeners" | awk '{n=split($4,a,":"); print a[n]}')"
  for port in "${ports[@]}"; do
    if printf '%s\n' "$listeners" | grep -Fxq "$port"; then
      if [[ "$port" == "$AGENT_PORT" && "$port" == "$old_agent_port" && "$agent_running" == true ]]; then
        continue
      fi
      if [[ "$port" != "$AGENT_PORT" ]] && printf '%s\n' "$docker_ports" | grep -Fxq "$port"; then
        continue
      fi
      refuse_existing "selected port $port is already in use outside the managed services"
    fi
  done
}

[[ "$(uname -s)" == Linux ]] || die 'Full-stack deployment is supported on Linux only'
((EUID != 0)) || die 'Run as the service user, not root; sudo is used only for host dependencies'
command -v curl >/dev/null 2>&1 || die 'curl is required'
[[ "$SERVICE" =~ ^[A-Za-z0-9_-]+$ ]] || die 'Invalid service name'
[[ "$IMAGE" =~ ^[A-Za-z0-9._/:@-]+$ ]] || die 'Invalid SnowLuma image reference'

if [[ "$ASSUME_YES" != true ]]; then
  ROOT_DIR="$(prompt_value 'Deployment root' "$ROOT_DIR")"
fi
[[ "$ROOT_DIR" = /* ]] || die '--root-dir must be an absolute path'
[[ "$ROOT_DIR" != *[[:space:]%\"]* ]] || die 'Deployment root contains unsupported characters'
command -v realpath >/dev/null || die 'realpath is required'
ROOT_DIR="$(realpath -m -- "$ROOT_DIR")"
[[ "$ROOT_DIR" != / ]] || die 'The filesystem root cannot be used as the deployment root'

APP_DIR="$ROOT_DIR/app"
AGENT_DATA_DIR="$ROOT_DIR/data"
SNOWLUMA_DIR="$ROOT_DIR/snowluma"
SNOWLUMA_DATA_DIR="$SNOWLUMA_DIR/data"
ENV_FILE="$SNOWLUMA_DIR/.env"
COMPOSE_FILE="$SNOWLUMA_DIR/docker-compose.yml"
ACCESS_FILE="$ROOT_DIR/deployment-access.txt"

EXISTING_STACK=false
PREFLIGHT_NODE=""
check_local_ownership
if [[ "$EXISTING_STACK" == true ]]; then
  if [[ "$SERVICE_SET" != true ]]; then
    SERVICE="$(env_value "$ENV_FILE" QQ_AGENT_SERVICE)"; SERVICE="${SERVICE:-qq-agent-linux}"
  fi
  if [[ "$IMAGE_SET" != true ]]; then
    IMAGE="$(env_value "$ENV_FILE" SNOWLUMA_IMAGE)"; IMAGE="${IMAGE:-motricseven7/snowluma:v1.14.15}"
  fi
  if [[ "$AGENT_PORT_SET" != true ]]; then
    AGENT_PORT="$(env_value "$ENV_FILE" AGENT_PORT)"; AGENT_PORT="${AGENT_PORT:-3210}"
  fi
  if [[ "$SNOWLUMA_PORT_SET" != true ]]; then
    SNOWLUMA_PORT="$(env_value "$ENV_FILE" SNOWLUMA_WEBUI_HOST_PORT)"; SNOWLUMA_PORT="${SNOWLUMA_PORT:-5099}"
  fi
  if [[ "$NOVNC_PORT_SET" != true ]]; then
    NOVNC_PORT="$(env_value "$ENV_FILE" NOVNC_PORT)"; NOVNC_PORT="${NOVNC_PORT:-6081}"
  fi
  if [[ "$ONEBOT_HTTP_PORT_SET" != true ]]; then
    ONEBOT_HTTP_PORT="$(env_value "$ENV_FILE" ONEBOT_HTTP_PORT)"; ONEBOT_HTTP_PORT="${ONEBOT_HTTP_PORT:-3000}"
  fi
  if [[ "$ONEBOT_WS_PORT_SET" != true ]]; then
    ONEBOT_WS_PORT="$(env_value "$ENV_FILE" ONEBOT_WS_PORT)"; ONEBOT_WS_PORT="${ONEBOT_WS_PORT:-3001}"
  fi
  SNOWLUMA_SCREEN="$(env_value "$ENV_FILE" SNOWLUMA_SCREEN)"; SNOWLUMA_SCREEN="${SNOWLUMA_SCREEN:-1920x1080x24}"
  SNOWLUMA_LOG_LEVEL="$(env_value "$ENV_FILE" SNOWLUMA_LOG_LEVEL)"; SNOWLUMA_LOG_LEVEL="${SNOWLUMA_LOG_LEVEL:-info}"
fi
[[ "$SERVICE" =~ ^[A-Za-z0-9_-]+$ ]] || die 'Invalid service name'
[[ "$IMAGE" =~ ^[A-Za-z0-9._/:@-]+$ ]] || die 'Invalid SnowLuma image reference'
[[ "$SNOWLUMA_SCREEN" =~ ^[0-9]+x[0-9]+x[0-9]+$ ]] \
  || die "Invalid SNOWLUMA_SCREEN, expected WIDTHxHEIGHTxDEPTH: $SNOWLUMA_SCREEN"
[[ "$SNOWLUMA_LOG_LEVEL" =~ ^[A-Za-z]+$ ]] || die "Invalid SNOWLUMA_LOG_LEVEL: $SNOWLUMA_LOG_LEVEL"
# 镜像站列表：QQ_AGENT_IMAGE_MIRROR（逗号分隔）+ --image-mirror 追加。
# 刻意不给默认值 —— 镜像站由第三方提供，用哪家必须由用户自己决定。
IMAGE_MIRRORS=()
if [[ -n "${QQ_AGENT_IMAGE_MIRROR:-}" ]]; then
  IFS=',' read -r -a IMAGE_MIRRORS <<<"$QQ_AGENT_IMAGE_MIRROR"
fi
[[ -z "$IMAGE_MIRROR_ARG" ]] || IMAGE_MIRRORS+=("$IMAGE_MIRROR_ARG")
if [[ -n "${QQ_AGENT_IMAGE_MIRROR:-}" || -n "$IMAGE_MIRROR_ARG" ]]; then
  for mirror in "${IMAGE_MIRRORS[@]}"; do
    [[ "$mirror" =~ ^[A-Za-z0-9.-]+(:[0-9]+)?$ ]] || die "Invalid image mirror host: $mirror"
  done
fi

if [[ "$ASSUME_YES" != true ]]; then
  AGENT_PORT="$(prompt_value 'QQ Agent console port' "$AGENT_PORT")"
  SNOWLUMA_PORT="$(prompt_value 'SnowLuma WebUI port' "$SNOWLUMA_PORT")"
  NOVNC_PORT="$(prompt_value 'QQ login/noVNC port' "$NOVNC_PORT")"
  ONEBOT_HTTP_PORT="$(prompt_value 'OneBot HTTP port (localhost only)' "$ONEBOT_HTTP_PORT")"
  ONEBOT_WS_PORT="$(prompt_value 'OneBot WebSocket port (localhost only)' "$ONEBOT_WS_PORT")"
fi
for item in \
  "agent:$AGENT_PORT" "SnowLuma:$SNOWLUMA_PORT" "noVNC:$NOVNC_PORT" \
  "OneBot HTTP:$ONEBOT_HTTP_PORT" "OneBot WebSocket:$ONEBOT_WS_PORT"; do
  validate_port "${item%%:*} port" "${item##*:}"
done
ports=("$AGENT_PORT" "$SNOWLUMA_PORT" "$NOVNC_PORT" "$ONEBOT_HTTP_PORT" "$ONEBOT_WS_PORT")
[[ "$(printf '%s\n' "${ports[@]}" | sort -u | wc -l | tr -d ' ')" == 5 ]] \
  || die 'All five host ports must be different'

if [[ "$SOURCE_DIR" != "$APP_DIR" && ( "$APP_DIR" == "$SOURCE_DIR/"* || "$SOURCE_DIR" == "$APP_DIR/"* ) ]]; then
  die 'Source and application directories cannot contain each other; choose another --root-dir'
fi
check_host_ownership
if [[ "$CHECK_ONLY" == true ]]; then
  if [[ "$EXISTING_STACK" == true ]]; then
    printf 'Environment: managed stack. Ownership and port checks passed; no deployment changes made.\n'
  else
    printf 'Environment: fresh installation. Ownership and port checks passed; no deployment changes made.\n'
  fi
  exit 0
fi

# 旧值用到时现读，不把凭据长期留在变量里（少一份密钥驻留在进程环境与内存中）
stored_value() { env_value "$ENV_FILE" "$1"; }
if [[ "$EXISTING_STACK" == true && "$ROTATE_CREDENTIALS" != true && "$ASSUME_YES" != true ]]; then
  if confirm 'Rotate Agent, OneBot, SnowLuma and noVNC credentials?' false; then
    ROTATE_CREDENTIALS=true
  fi
fi
if [[ "$EXISTING_STACK" == true && "$ROTATE_CREDENTIALS" != true ]]; then
  AGENT_TOKEN="${AGENT_TOKEN:-$(stored_value QQ_AGENT_CONSOLE_TOKEN)}"
  ONEBOT_TOKEN="${ONEBOT_TOKEN:-$(stored_value ONEBOT_TOKEN)}"
  SNOWLUMA_PASSWORD="${SNOWLUMA_PASSWORD:-$(stored_value SNOWLUMA_WEBUI_BOOTSTRAP_PASSWORD)}"
  VNC_PASSWORD="${VNC_PASSWORD:-$(stored_value VNC_PASSWD)}"
fi

AGENT_TOKEN="${AGENT_TOKEN:-$(random_hex 24)}"
ONEBOT_TOKEN="${ONEBOT_TOKEN:-$(random_hex 32)}"
SNOWLUMA_PASSWORD="${SNOWLUMA_PASSWORD:-Sl-$(random_hex 12)!Aa}"
VNC_PASSWORD="${VNC_PASSWORD:-$(random_hex 4)}"

if [[ "$ASSUME_YES" != true ]] && [[ "$EXISTING_STACK" != true || "$ROTATE_CREDENTIALS" == true ]]; then
  if confirm 'Use custom credentials instead of the generated values?' false; then
    AGENT_TOKEN="$(prompt_secret 'QQ Agent console token')"
    ONEBOT_TOKEN="$(prompt_secret 'Shared OneBot token')"
    SNOWLUMA_PASSWORD="$(prompt_secret 'SnowLuma WebUI password')"
    VNC_PASSWORD="$(prompt_secret 'noVNC password (8 characters recommended)')"
  fi
fi

if [[ "$EXISTING_STACK" == true && "$SNOWLUMA_PASSWORD" != "$(stored_value SNOWLUMA_WEBUI_BOOTSTRAP_PASSWORD)" ]]; then
  SNOWLUMA_CURRENT_PASSWORD="${SNOWLUMA_CURRENT_PASSWORD:-$(stored_value SNOWLUMA_WEBUI_BOOTSTRAP_PASSWORD)}"
  if [[ -z "$SNOWLUMA_CURRENT_PASSWORD" && "$ASSUME_YES" != true ]]; then
    SNOWLUMA_CURRENT_PASSWORD="$(prompt_secret 'Current SnowLuma WebUI password')"
  fi
  [[ -n "$SNOWLUMA_CURRENT_PASSWORD" ]] \
    || die 'The current SnowLuma password is required to rotate existing credentials'
fi

if [[ "$EXISTING_STACK" != true && "$SKIP_MODEL_CONFIG" != true ]]; then
  if [[ "$ASSUME_YES" != true ]]; then
    MODEL_BASE_URL="$(prompt_value 'Model API base URL' "${MODEL_BASE_URL:-https://api.deepseek.com}")"
    MODEL_NAME="$(prompt_value 'Model name' "${MODEL_NAME:-deepseek-chat}")"
    if [[ -z "$MODEL_API_KEY" ]]; then
      MODEL_API_KEY="$(prompt_secret 'Model API key')"
    fi
    ALLOW_GROUPS="$(prompt_value 'Allowed group IDs, comma-separated (blank to configure later)' "$ALLOW_GROUPS")"
    ALLOW_PRIVATE="$(prompt_value 'Allowed private QQ IDs, comma-separated (blank to configure later)' "$ALLOW_PRIVATE")"
  fi
  [[ -n "$MODEL_BASE_URL" && -n "$MODEL_API_KEY" && -n "$MODEL_NAME" ]] \
    || die 'Fresh non-interactive deployment requires --model-base-url, --model-api-key and --model, or --skip-model-config'
fi
if [[ -n "$MODEL_BASE_URL" && ! "$MODEL_BASE_URL" =~ ^https?:// ]] ; then
  die 'Model API base URL must use http:// or https://'
fi
for list in "$ALLOW_GROUPS" "$ALLOW_PRIVATE"; do
  [[ -z "$list" || "$list" =~ ^[0-9]+(,[0-9]+)*$ ]] \
    || die 'Allowlists must contain comma-separated numeric QQ IDs without spaces'
done

validate_secret 'QQ Agent console token' "$AGENT_TOKEN" 16
validate_secret 'OneBot token' "$ONEBOT_TOKEN" 16
validate_secret 'SnowLuma WebUI password' "$SNOWLUMA_PASSWORD" 10
[[ "$SNOWLUMA_PASSWORD" =~ [a-z] && "$SNOWLUMA_PASSWORD" =~ [A-Z] \
  && "$SNOWLUMA_PASSWORD" =~ [^A-Za-z0-9] ]] \
  || die 'SnowLuma WebUI password must contain lowercase, uppercase, and a special character'
validate_secret 'noVNC password' "$VNC_PASSWORD" 8

step 'Preparing directories'
if [[ ! -d "$ROOT_DIR" ]]; then
  run_root mkdir -p "$ROOT_DIR"
  run_root chown "$(id -u):$(id -g)" "$ROOT_DIR"
fi
[[ -w "$ROOT_DIR" ]] || die "Deployment root is not writable by $(id -un): $ROOT_DIR"
mkdir -p "$APP_DIR" "$AGENT_DATA_DIR" \
  "$SNOWLUMA_DATA_DIR/config" "$SNOWLUMA_DIR/client-config" "$SNOWLUMA_DIR/client-data"
chmod 700 "$ROOT_DIR" "$AGENT_DATA_DIR" "$SNOWLUMA_DIR" "$SNOWLUMA_DATA_DIR" \
  "$SNOWLUMA_DIR/client-config" "$SNOWLUMA_DIR/client-data"

if [[ -f "$ENV_FILE" ]]; then
  cp -p "$ENV_FILE" "$ENV_FILE.pre-deploy"
fi
cat >"$ENV_FILE.tmp" <<EOF
SNOWLUMA_IMAGE=$IMAGE
SNOWLUMA_CONTAINER=qq-agent-snowluma
AGENT_PORT=$AGENT_PORT
QQ_AGENT_SERVICE=$SERVICE
SNOWLUMA_UID=$(id -u)
SNOWLUMA_GID=$(id -g)
SNOWLUMA_WEBUI_HOST=0.0.0.0
SNOWLUMA_WEBUI_PORT=5099
SNOWLUMA_WEBUI_HOST_PORT=$SNOWLUMA_PORT
SNOWLUMA_WEBUI_BOOTSTRAP_PASSWORD=$SNOWLUMA_PASSWORD
SNOWLUMA_LOG_LEVEL=$SNOWLUMA_LOG_LEVEL
SNOWLUMA_SCREEN=$SNOWLUMA_SCREEN
SNOWLUMA_HOOK_AUTOLOAD=1
SNOWLUMA_ONEBOT_HOST=0.0.0.0
SNOWLUMA_TELEMETRY=0
VNC_PASSWD=$VNC_PASSWORD
NOVNC_PORT=$NOVNC_PORT
ONEBOT_HTTP_PORT=$ONEBOT_HTTP_PORT
ONEBOT_WS_PORT=$ONEBOT_WS_PORT
ONEBOT_TOKEN=$ONEBOT_TOKEN
QQ_AGENT_CONSOLE_TOKEN=$AGENT_TOKEN
EOF
mv "$ENV_FILE.tmp" "$ENV_FILE"
chmod 600 "$ENV_FILE"

cat >"$COMPOSE_FILE.tmp" <<'EOF'
services:
  snowluma:
    image: "${SNOWLUMA_IMAGE}"
    container_name: "${SNOWLUMA_CONTAINER}"
    restart: unless-stopped
    shm_size: 1gb
    ulimits:
      nofile:
        soft: 65536
        hard: 1048576
    cap_add:
      - SYS_PTRACE
    security_opt:
      - seccomp=unconfined
    environment:
      VNC_PASSWD: "${VNC_PASSWD}"
      SNOWLUMA_ONEBOT_HOST: "${SNOWLUMA_ONEBOT_HOST}"
      SNOWLUMA_UID: "${SNOWLUMA_UID}"
      SNOWLUMA_GID: "${SNOWLUMA_GID}"
      SNOWLUMA_WEBUI_HOST: "${SNOWLUMA_WEBUI_HOST}"
      SNOWLUMA_WEBUI_PORT: "${SNOWLUMA_WEBUI_PORT}"
      SNOWLUMA_WEBUI_BOOTSTRAP_PASSWORD: "${SNOWLUMA_WEBUI_BOOTSTRAP_PASSWORD}"
      SNOWLUMA_LOG_LEVEL: "${SNOWLUMA_LOG_LEVEL}"
      SNOWLUMA_SCREEN: "${SNOWLUMA_SCREEN}"
      SNOWLUMA_HOOK_AUTOLOAD: "${SNOWLUMA_HOOK_AUTOLOAD}"
      SNOWLUMA_QQ_FLAGS: --disable-gpu --disable-software-rasterizer --disable-gpu-compositing
      SNOWLUMA_TELEMETRY: "${SNOWLUMA_TELEMETRY}"
    ports:
      - "0.0.0.0:${NOVNC_PORT}:6081"
      - "0.0.0.0:${SNOWLUMA_WEBUI_HOST_PORT}:5099"
      - "127.0.0.1:${ONEBOT_HTTP_PORT}:3000"
      - "127.0.0.1:${ONEBOT_WS_PORT}:3001"
    volumes:
      - ./data:/app/data
      - ./client-config:/app/.config
      - ./client-data:/app/.local/share
EOF
mv "$COMPOSE_FILE.tmp" "$COMPOSE_FILE"
chmod 600 "$COMPOSE_FILE"

# 从这里到 Agent 部署成功之间失败的话，栈是"半成品"：check_local_ownership 会因此拒绝重跑，
# 所以先挂上清理钩子（Agent 装好后立刻撤销）。
if [[ "$EXISTING_STACK" != true ]]; then
  FRESH_STACK_CLEANUP=true
  trap cleanup_fresh_stack EXIT
fi

if ! docker_ready && ! sudo_docker_ready; then install_docker; fi
docker_ready || sudo_docker_ready || die 'Docker installation completed but the daemon is unavailable'
docker_call compose version >/dev/null 2>&1 || die 'Docker Compose v2 is required'

# 拉一个引用：每个地址最多 3 次，退避 3 / 6 秒
pull_with_retry() {
  local ref="$1" attempt
  for attempt in 1 2 3; do
    if docker_call pull "$ref"; then return 0; fi
    ((attempt < 3)) || return 1
    sleep $((attempt * 3))
  done
  return 1
}

# 换成镜像站之后同步 .env：compose 走 --env-file，里面也得是同一个引用；权限保持不变
update_env_image() {
  local tmp="$ENV_FILE.tmp.$$"
  awk -v image="$IMAGE" -F= 'BEGIN { OFS="=" } $1 == "SNOWLUMA_IMAGE" { print "SNOWLUMA_IMAGE=" image; next } { print }' \
    "$ENV_FILE" >"$tmp" || return 1
  chmod --reference="$ENV_FILE" "$tmp" 2>/dev/null || chmod 600 "$tmp"
  mv "$tmp" "$ENV_FILE"
}

# 连不上 Docker Hub 时的落地方案（国内服务器常见；镜像站是第三方，脚本不替用户选）
image_pull_hint() {
  cat >&2 <<'HINT'

拉取 SnowLuma 镜像失败。国内/受限网络连不上 Docker Hub（registry-1.docker.io）很常见，
下面三种办法任选一种，然后重跑 deploy-all.sh：

  1) 换镜像站重跑（<mirror> 换成你信得过的加速器地址，例如云厂商给的那个）：
       QQ_AGENT_IMAGE_MIRROR=<mirror> bash deploy-all.sh
     或直接给完整地址：
       bash deploy-all.sh --image <mirror>/motricseven7/snowluma:v1.14.15

  2) 给 Docker 配全局加速器（配一次，之后所有拉取都走它）：
       sudo tee /etc/docker/daemon.json <<'JSON'
       { "registry-mirrors": ["https://<你的加速器地址>"] }
       JSON
       sudo systemctl restart docker

  3) 在能联网的机器上拉好再带过来：
       docker pull motricseven7/snowluma:v1.14.15
       docker save motricseven7/snowluma:v1.14.15 | gzip > snowluma.tgz
       gunzip -c snowluma.tgz | docker load     # 在目标机上执行

镜像站由第三方提供，脚本不会替你默认选任何一家。
HINT
}

step "Downloading SnowLuma image $IMAGE"
PULLED=false
if pull_with_retry "$IMAGE"; then
  PULLED=true
else
  for mirror in "${IMAGE_MIRRORS[@]}"; do
    # 只在 IMAGE 还没有 registry host 时加前缀：上一次走过镜像站之后 .env 里存的就是
    # 带前缀的完整引用，再加一次会拼成 mirror/mirror/... 这种拉不到的地址。
    case "$IMAGE" in
      */*/*) candidate="$IMAGE" ;;
      *) candidate="${mirror%/}/$IMAGE" ;;
    esac
    step "Retrying through image mirror $mirror"
    if pull_with_retry "$candidate"; then
      IMAGE="$candidate"
      update_env_image || true
      PULLED=true
      break
    fi
  done
fi
if [[ "$PULLED" != true ]]; then
  image_pull_hint
  die "Failed to download SnowLuma image ($IMAGE); see the hint above"
fi

step 'Installing QQ Agent'
export QQ_AGENT_CONSOLE_TOKEN="$AGENT_TOKEN"
export QQ_AGENT_ONEBOT_TOKEN="$ONEBOT_TOKEN"
export QQ_AGENT_ONEBOT_HTTP_TOKEN="$ONEBOT_TOKEN"
export QQ_AGENT_ONEBOT_HTTP_URL="http://127.0.0.1:$ONEBOT_HTTP_PORT"
export QQ_AGENT_ONEBOT_WS_URL="ws://127.0.0.1:$ONEBOT_WS_PORT"
export QQ_SNOWLUMA_WEBUI_URL="http://127.0.0.1:$SNOWLUMA_PORT"
[[ -z "$MODEL_BASE_URL" ]] || export QQ_AGENT_MODEL_BASE_URL="$MODEL_BASE_URL"
# 模型 Key 不走子进程环境（/proc/<pid>/environ 里读得到）：写 0600 临时文件，
# 部署脚本按 QQ_AGENT_MODEL_KEY_FILE 读，脚本退出时删掉。
if [[ -n "$MODEL_API_KEY" ]]; then
  MODEL_KEY_FILE="$(mktemp "${TMPDIR:-/tmp}/qq-agent-model-key.XXXXXX")"
  printf '%s' "$MODEL_API_KEY" >"$MODEL_KEY_FILE"
  chmod 600 "$MODEL_KEY_FILE"
  export QQ_AGENT_MODEL_KEY_FILE="$MODEL_KEY_FILE"
  trap 'rm -f "$MODEL_KEY_FILE"; cleanup_fresh_stack' EXIT
fi
[[ -z "$MODEL_NAME" ]] || export QQ_AGENT_MODEL="$MODEL_NAME"
[[ -z "$ALLOW_GROUPS" ]] || export QQ_AGENT_ALLOW_GROUPS="$ALLOW_GROUPS"
[[ -z "$ALLOW_PRIVATE" ]] || export QQ_AGENT_ALLOW_PRIVATE="$ALLOW_PRIVATE"
bash "$SOURCE_DIR/deploy.sh" \
  --install-dir "$APP_DIR" \
  --data-dir "$AGENT_DATA_DIR" \
  --host 0.0.0.0 \
  --port "$AGENT_PORT" \
  --service "$SERVICE"
# Agent 已装好，栈不再是"半成品"：撤销失败清理，保留 .env / compose。
FRESH_STACK_CLEANUP=false

NODE_BIN="$(tr -d '\r\n' <"$APP_DIR/.deployment-node")"
if [[ "$EXISTING_STACK" != true ]]; then
  "$APP_DIR/manage.sh" observe
fi
"$NODE_BIN" "$SOURCE_DIR/scripts/configure-snowluma.mjs" \
  --data-dir "$SNOWLUMA_DATA_DIR" \
  --http-port 3000 \
  --ws-port 3001

step 'Starting SnowLuma and OneBot'
(cd "$SNOWLUMA_DIR" && docker_call compose --env-file .env up -d)
wait_http "http://127.0.0.1:$SNOWLUMA_PORT/api/ui/public" 90 \
  || die "SnowLuma WebUI did not become ready; run: cd $SNOWLUMA_DIR && docker compose logs"
wait_http "http://127.0.0.1:$NOVNC_PORT/" 30 \
  || die "noVNC did not become ready; run: cd $SNOWLUMA_DIR && docker compose logs"
if [[ "$EXISTING_STACK" == true && "$SNOWLUMA_PASSWORD" != "$(stored_value SNOWLUMA_WEBUI_BOOTSTRAP_PASSWORD)" \
  && "$SNOWLUMA_CURRENT_PASSWORD" != "$SNOWLUMA_PASSWORD" ]]; then
  # 密码走环境变量而不是命令行参数：/proc/<pid>/cmdline 对本机所有用户可读。
  export QQ_AGENT_SNOWLUMA_CURRENT_PASSWORD="$SNOWLUMA_CURRENT_PASSWORD"
  export QQ_AGENT_SNOWLUMA_PASSWORD="$SNOWLUMA_PASSWORD"
  rotate_args=(--url "http://127.0.0.1:$SNOWLUMA_PORT")
  [[ -z "$SNOWLUMA_TOTP" ]] || rotate_args+=(--totp "$SNOWLUMA_TOTP")
  if ! "$NODE_BIN" "$SOURCE_DIR/scripts/rotate-snowluma-password.mjs" "${rotate_args[@]}"; then
    cp -p "$ENV_FILE.pre-deploy" "$ENV_FILE"
    die 'SnowLuma password rotation failed; the previous stack environment was restored'
  fi
fi
systemctl --user restart "$SERVICE.service"
wait_http "http://127.0.0.1:$AGENT_PORT/healthz" 30 \
  || die "QQ Agent did not become ready; run: $APP_DIR/manage.sh logs"

HOST_IP="$(hostname -I 2>/dev/null | awk '{print $1}' || true)"
HOST_IP="${HOST_IP:-127.0.0.1}"
MODE="$("$NODE_BIN" -e 'const c=require(process.argv[1]);process.stdout.write(c.runtime.mode)' \
  "$AGENT_DATA_DIR/config.json")"
cat >"$ACCESS_FILE.tmp" <<EOF
QQ Agent full-stack access
Agent console: http://$HOST_IP:$AGENT_PORT
Agent token: $AGENT_TOKEN
SnowLuma WebUI: http://$HOST_IP:$SNOWLUMA_PORT
SnowLuma password: $SNOWLUMA_PASSWORD
QQ login/noVNC: http://$HOST_IP:$NOVNC_PORT
noVNC password: $VNC_PASSWORD
OneBot HTTP: http://127.0.0.1:$ONEBOT_HTTP_PORT
OneBot WebSocket: ws://127.0.0.1:$ONEBOT_WS_PORT
OneBot token: $ONEBOT_TOKEN
Mode: $MODE
EOF
mv "$ACCESS_FILE.tmp" "$ACCESS_FILE"
chmod 600 "$ACCESS_FILE"

printf '\nDeployment complete. Agent mode: %s.\n' "$MODE"
printf '1. Open QQ login:     http://%s:%s\n' "$HOST_IP" "$NOVNC_PORT"
printf '2. Scan the QR code and finish QQ login.\n'
printf '3. Open Agent console: http://%s:%s\n' "$HOST_IP" "$AGENT_PORT"
printf '4. Verify OneBot is connected in the Agent console.\n'
if [[ "$SKIP_MODEL_CONFIG" == true || -z "$ALLOW_GROUPS$ALLOW_PRIVATE" ]]; then
  printf '5. Complete the model/allowlist fields that were intentionally left blank.\n'
fi
printf '6. Activate only after excluding the old bot: %s/manage.sh activate --confirm-exclusive\n' "$APP_DIR"
printf 'Credentials: %s\n' "$ACCESS_FILE"

if [[ "$ASSUME_YES" != true ]]; then
  printf '\nComplete QQ login in noVNC, then press Enter to verify the OneBot connection: ' >/dev/tty
  IFS= read -r _ </dev/tty || true
  CONNECTED=false
  for _ in {1..30}; do
    if onebot_ready; then CONNECTED=true; break; fi
    sleep 1
  done
  if [[ "$CONNECTED" == true ]]; then
    printf 'OneBot login verified.\n'
    if [[ "$MODE" != active && "$SKIP_MODEL_CONFIG" != true && -n "$ALLOW_GROUPS$ALLOW_PRIVATE" ]] \
      && confirm 'Activate QQ Agent now? Confirm the old bot no longer handles these chats.' false; then
      "$APP_DIR/manage.sh" activate --confirm-exclusive
    fi
  else
    printf 'OneBot is not logged in yet. The services remain installed in observe mode.\n' >&2
  fi
fi
