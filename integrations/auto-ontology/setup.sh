#!/bin/sh
set -eu
umask 077

integration_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
repo_dir=$(CDPATH= cd -- "$integration_dir/../.." && pwd -P)
upstream_commit=fc43a82600e18b19d159c52023f54f5572f537ac
env_file="$integration_dir/.env"
client_env="$integration_dir/fieldkit.env"

compose() { docker compose --env-file "$env_file" -f "$integration_dir/compose.yaml" "$@"; }
infra() { docker compose --env-file "$env_file" -f "$integration_dir/compose.yaml" -f "$integration_dir/compose.infra.yaml" "$@"; }
setting() { awk -v key="$1" 'index($0,key"=")==1 { print substr($0,length(key)+2); exit }' "$env_file"; }
check_source() {
  source_dir=$(setting AUTO_ONTOLOGY_SOURCE)
  [ "$(git -C "$source_dir" rev-parse HEAD)" = "$upstream_commit" ] || { printf 'The Auto Ontology checkout must be pinned to %s.\n' "$upstream_commit" >&2; exit 1; }
  git -C "$source_dir" diff --quiet && git -C "$source_dir" diff --cached --quiet || { printf 'The pinned upstream source contains local edits; use a clean checkout.\n' >&2; exit 1; }
}
require_key() {
  [ -n "$(setting NVIDIA_API_KEY)" ] || [ -n "$(setting DEFAULT_MODELS_API_KEY)" ] || { printf 'Set NVIDIA_API_KEY in %s before starting inference.\n' "$env_file" >&2; exit 1; }
}
load_token() {
  AUTO_ONTOLOGY_API_TOKEN=
  if [ -f "$client_env" ]; then AUTO_ONTOLOGY_API_TOKEN=$(awk 'index($0,"AUTO_ONTOLOGY_API_TOKEN=")==1 {print substr($0,25); exit}' "$client_env"); fi
  export AUTO_ONTOLOGY_API_TOKEN
}
semantic_control() {
  { cat "$integration_dir/semantic-control.mjs"; printf '\nawait control();\n'; } |
    compose exec -T -e AUTO_ONTOLOGY_API_TOKEN -e FIELDKIT_AO_CONTROL_ACTION -e FIELDKIT_AO_COMPILE_SINCE -e FIELDKIT_AO_SETUP_TIMEOUT frontend node --input-type=module
}
freeze_service() {
  load_token
  FIELDKIT_AO_CONTROL_ACTION=freeze
  export FIELDKIT_AO_CONTROL_ACTION
  freeze_result=0
  semantic_control || freeze_result=$?
  # Always stop the worker, including when the gateway is unavailable. A stopped
  # container cannot perform its next scheduled embedding/compilation pass.
  compose stop ingestion-service || freeze_result=$?
  return "$freeze_result"
}
connect_cleanup() {
  exit_result=$?
  trap - EXIT HUP INT TERM
  if [ -n "${client_tmp:-}" ]; then rm -f "$client_tmp"; fi
  if [ "${token_only:-1}" = 0 ]; then
    cleanup_result=0
    freeze_service || cleanup_result=$?
    if [ "$exit_result" = 0 ]; then exit_result=$cleanup_result; fi
  fi
  exit "$exit_result"
}

case "${1:-help}" in
  init)
    if [ -e "$env_file" ]; then printf 'Existing private settings retained: %s\n' "$env_file"; exit 0; fi
    command -v git >/dev/null
    command -v openssl >/dev/null
    source_dir=${AUTO_ONTOLOGY_SOURCE:-"$integration_dir/.local/source"}
    case "$(uname -m)" in arm64|aarch64) native_platform=linux/arm64;; *) native_platform=linux/amd64;; esac
    mkdir -p "$integration_dir/.local" "$integration_dir/data"
    if [ ! -d "$source_dir/.git" ]; then git clone https://github.com/NVIDIA/auto-ontology.git "$source_dir"; fi
    if [ "$(git -C "$source_dir" rev-parse HEAD)" != "$upstream_commit" ]; then
      if [ -n "$(git -C "$source_dir" status --porcelain)" ]; then printf 'The upstream checkout has local changes; use a separate clean clone.\n' >&2; exit 1; fi
      git -C "$source_dir" fetch origin "$upstream_commit"
      git -C "$source_dir" checkout --detach "$upstream_commit"
    fi
    case "$source_dir$integration_dir" in *'
'*) printf 'Paths must not contain newlines.\n' >&2; exit 1;; esac
    cp "$integration_dir/.env.example" "$env_file"
    # Generated values contain only hex/digits; paths never become shell code.
    {
      printf '\n# Generated private overrides\n'
      printf 'AUTO_ONTOLOGY_SOURCE=%s\n' "$source_dir"
      printf 'FIELDKIT_PROJECTION_DIR=%s/data\n' "$integration_dir"
      printf 'FIELDKIT_AO_UID=%s\n' "$(id -u)"
      printf 'FIELDKIT_AO_GID=%s\n' "$(id -g)"
      printf 'FIELDKIT_AO_PLATFORM=%s\n' "$native_platform"
      printf 'POSTGRES_PASSWORD=%s\n' "$(openssl rand -hex 24)"
      printf 'AUTH_SECRET=%s\n' "$(openssl rand -hex 32)"
      printf 'AUTO_ONTOLOGY_ADMIN_PASSWORD=%s\n' "$(openssl rand -hex 24)"
    } >> "$env_file"
    # Keep exactly one assignment per key so edits and environment readers agree.
    awk 'BEGIN {FS="="} /^[A-Za-z_][A-Za-z0-9_]*=/ {line[$1]=$0; next} {other[++n]=$0} END {for(i=1;i<=n;i++) print other[i]; for(k in line) print line[k]}' "$env_file" > "$env_file.tmp"
    mv "$env_file.tmp" "$env_file"
    chmod 600 "$env_file"
    chmod 700 "$integration_dir/.local" "$integration_dir/data"
    printf 'Private settings created: %s\nSet NVIDIA_API_KEY there before starting inference.\n' "$env_file"
    ;;
  build)
    [ -f "$env_file" ] || { printf 'Run setup.sh init first.\n' >&2; exit 1; }
    check_source
    compose config --quiet
    compose build backend-migrate frontend-migrate
    ;;
  start-infra)
    [ -f "$env_file" ] || { printf 'Run setup.sh init first.\n' >&2; exit 1; }
    check_source
    projection_dir=$(setting FIELDKIT_PROJECTION_DIR)
    if [ -f "$projection_dir/fieldkit.sqlite" ]; then
      FIELDKIT_AO_INFRA_CONNECTION_STRINGS=sqlite:////fieldkit/fieldkit.sqlite
      export FIELDKIT_AO_INFRA_CONNECTION_STRINGS
    fi
    infra config --quiet
    infra stop ingestion-service
    infra up -d postgres
    infra run --rm backend-migrate
    infra run --rm frontend-migrate
    infra up -d --wait --wait-timeout 120 --no-deps auto-ontology
    infra up -d --wait --wait-timeout 120 --no-deps frontend
    printf 'Authentication infrastructure started at %s; ingestion and external inference are disabled.\n' "$(setting APP_URL)"
    ;;
  catalog-infra)
    [ -f "$env_file" ] || { printf 'Run setup.sh init first.\n' >&2; exit 1; }
    check_source
    projection_dir=$(setting FIELDKIT_PROJECTION_DIR)
    [ -f "$projection_dir/fieldkit.sqlite" ] || { printf 'Export the normalized projection with fieldkit nvidia export first.\n' >&2; exit 1; }
    FIELDKIT_AO_INFRA_CONNECTION_STRINGS=sqlite:////fieldkit/fieldkit.sqlite
    export FIELDKIT_AO_INFRA_CONNECTION_STRINGS
    infra stop ingestion-service
    infra up -d --wait --wait-timeout 120 --no-deps auto-ontology
    infra exec -T auto-ontology python - < "$integration_dir/catalog-infra.py"
    printf 'Projection source and real NVIDIA catalog configured; embedding and semantic compilation remain disabled.\n'
    ;;
  start)
    [ -f "$env_file" ] || { printf 'Run setup.sh init first.\n' >&2; exit 1; }
    check_source
    require_key
    projection_dir=$(setting FIELDKIT_PROJECTION_DIR)
    if [ ! -f "$projection_dir/fieldkit.sqlite" ]; then
      [ -x "$repo_dir/fieldkit" ] || { printf 'Build Fieldkit first, then export the NVIDIA projection.\n' >&2; exit 1; }
      FIELDKIT_AUTO_ONTOLOGY_PROJECTION="$projection_dir/fieldkit.sqlite" "$repo_dir/fieldkit" --case "${FIELDKIT_CASE:-$repo_dir/cases/my-investigation}" nvidia export
    fi
    compose config --quiet
    compose build backend-migrate frontend-migrate
    compose stop ingestion-service
    compose up -d postgres
    compose run --rm backend-migrate
    compose run --rm frontend-migrate
    compose up -d --wait --wait-timeout 120 --no-deps auto-ontology
    compose up -d --wait --wait-timeout 120 --no-deps frontend
    printf 'Local Auto Ontology gateway: %s\nIngestion is stopped. Run setup.sh connect for one initial embedding/compilation pass.\n' "$(setting APP_URL)"
    ;;
  connect|connect-infra)
    token_only=0
    [ "$1" != connect-infra ] || token_only=1
    client_tmp=
    trap connect_cleanup EXIT
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM
    if [ "$token_only" = 0 ]; then
      require_key
      projection_dir=$(setting FIELDKIT_PROJECTION_DIR)
      [ -f "$projection_dir/fieldkit.sqlite" ] || { printf 'Export the Fieldkit projection first.\n' >&2; exit 1; }
      FIELDKIT_AO_COMPILE_SINCE=$(date -u +%Y-%m-%dT%H:%M:%SZ)
      FIELDKIT_AO_SETUP_TIMEOUT=${FIELDKIT_AO_SETUP_TIMEOUT:-$(setting FIELDKIT_AO_SETUP_TIMEOUT)}
      FIELDKIT_AO_SETUP_TIMEOUT=${FIELDKIT_AO_SETUP_TIMEOUT:-1800}
      case "$FIELDKIT_AO_SETUP_TIMEOUT" in ''|*[!0-9]*) printf 'Setup timeout must be 1–7200 seconds.\n' >&2; exit 1;; esac
      [ "$FIELDKIT_AO_SETUP_TIMEOUT" -ge 1 ] && [ "$FIELDKIT_AO_SETUP_TIMEOUT" -le 7200 ] || { printf 'Setup timeout must be 1–7200 seconds.\n' >&2; exit 1; }
      export FIELDKIT_AO_COMPILE_SINCE FIELDKIT_AO_SETUP_TIMEOUT
      compose up -d --wait --wait-timeout 120 --no-deps auto-ontology
      freeze_service
      compose up -d --no-deps ingestion-service
    fi
    if [ -e "$client_env" ]; then
      if [ "$token_only" = 0 ]; then
        AUTO_ONTOLOGY_API_TOKEN=$(awk 'index($0,"AUTO_ONTOLOGY_API_TOKEN=")==1 {print substr($0,25); exit}' "$client_env")
        export AUTO_ONTOLOGY_API_TOKEN
        compose exec -T -e AUTO_ONTOLOGY_API_TOKEN -e FIELDKIT_AO_ENABLE_ONLY=1 frontend node --input-type=module < "$integration_dir/bootstrap-token.mjs"
        unset AUTO_ONTOLOGY_API_TOKEN
      fi
      printf 'Existing client token retained: %s\n' "$client_env"
    else
      projection_dir=$(setting FIELDKIT_PROJECTION_DIR)
      client_tmp=$(mktemp "$integration_dir/.local/fieldkit.env.XXXXXX")
      compose exec -T -e FIELDKIT_AO_TOKEN_ONLY="$token_only" frontend node --input-type=module < "$integration_dir/bootstrap-token.mjs" > "$client_tmp"
      printf 'FIELDKIT_AUTO_ONTOLOGY_PROJECTION=%s/fieldkit.sqlite\n' "$projection_dir" >> "$client_tmp"
      chmod 600 "$client_tmp"
      mv "$client_tmp" "$client_env"
      client_tmp=
      printf 'Private Fieldkit connection saved: %s\n' "$client_env"
    fi
    if [ "$token_only" = 1 ]; then printf 'Token-only bootstrap completed; ingestion and inference remain disabled.\n';
    else
      load_token
      FIELDKIT_AO_CONTROL_ACTION=wait
      export FIELDKIT_AO_CONTROL_ACTION
      semantic_control
      printf 'Initial compilation is ready. Background work will now be frozen; explicit Fieldkit queries remain available.\n'
    fi
    ;;
  freeze)
    freeze_service
    printf 'Ingestion stopped. The gateway, source connection, catalog and retained semantic layer remain available.\n'
    ;;
  status) compose ps ;;
  logs) compose logs --tail=80 "${2:-auto-ontology}" ;;
  stop) compose down ;;
  *)
    printf 'Usage: %s init|build|start-infra|connect-infra|catalog-infra|start|connect|freeze|status|logs [service]|stop\n' "$0"
    printf 'init creates private settings; build installs the service; start needs a configured inference key.\n'
    ;;
esac
