#!/usr/bin/env bash
set -Eeuo pipefail
umask 077

: "${BACKUP_FILE:?Set BACKUP_FILE to the downloaded encrypted cluster dump}"
: "${AGE_IDENTITY_FILE:?Set AGE_IDENTITY_FILE to the separately held private age identity}"
: "${RESTORE_ENV_FILE:?Set RESTORE_ENV_FILE to the owner-only restore Compose env file}"
: "${RESTORE_ISOLATION_ACK:?Set RESTORE_ISOLATION_ACK=true after confirming this is a separate isolated recovery environment}"

[[ "$RESTORE_ISOLATION_ACK" == true ]] || { printf '%s\n' 'Refusing restore without explicit recovery-isolation confirmation.' >&2; exit 2; }
[[ -f "$BACKUP_FILE" && ! -L "$BACKUP_FILE" ]] || { printf '%s\n' 'Backup must be a regular non-symlink file.' >&2; exit 2; }
[[ -f "$BACKUP_FILE.sha256" && ! -L "$BACKUP_FILE.sha256" ]] || { printf '%s\n' 'The encrypted backup checksum sidecar is required.' >&2; exit 2; }
[[ -f "$AGE_IDENTITY_FILE" && ! -L "$AGE_IDENTITY_FILE" ]] || { printf '%s\n' 'Age identity must be a regular non-symlink file.' >&2; exit 2; }
[[ "$AGE_IDENTITY_FILE" == /* && "$RESTORE_ENV_FILE" == /* ]] || { printf '%s\n' 'Identity and env-file paths must be absolute.' >&2; exit 2; }
[[ -f "$RESTORE_ENV_FILE" && ! -L "$RESTORE_ENV_FILE" ]] || { printf '%s\n' 'Restore env file must be a regular non-symlink file.' >&2; exit 2; }
identity_mode=$(stat -c '%a' -- "$AGE_IDENTITY_FILE")
env_mode=$(stat -c '%a' -- "$RESTORE_ENV_FILE")
[[ "$identity_mode" == 600 || "$identity_mode" == 400 ]] || { printf '%s\n' 'Age identity file must be mode 0600 or 0400.' >&2; exit 2; }
[[ "$env_mode" == 600 || "$env_mode" == 400 ]] || { printf '%s\n' 'Restore env file must be mode 0600 or 0400.' >&2; exit 2; }

command -v age >/dev/null
command -v docker >/dev/null
command -v sha256sum >/dev/null

backup_dir=$(CDPATH= cd -- "$(dirname -- "$BACKUP_FILE")" && pwd)
backup_name=$(basename -- "$BACKUP_FILE")
(
  cd -- "$backup_dir"
  sha256sum --check -- "$backup_name.sha256"
)
# Verify the age authentication tag without writing decrypted data to disk.
age --decrypt --identity "$AGE_IDENTITY_FILE" "$BACKUP_FILE" >/dev/null

script_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
compose_file="$script_dir/restore.compose.yml"
stamp=$(date -u +%Y%m%d%H%M%S)
project="medical-assistant-restore-${stamp}-$$"
export RESTORE_BOOTSTRAP_DB="restore_import_${stamp}_$$"
compose=(docker compose --env-file "$RESTORE_ENV_FILE" --project-name "$project" --file "$compose_file")
started=true
cleanup() {
  status=$?
  if [[ $status -ne 0 && $started == true ]]; then
    "${compose[@]}" down --remove-orphans >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

"${compose[@]}" up -d --wait restore-db

# Restore the full PostgreSQL cluster into a fresh tmpfs database, never into production.
age --decrypt --identity "$AGE_IDENTITY_FILE" "$BACKUP_FILE" \
  | "${compose[@]}" exec -T restore-db sh -ec 'PGPASSWORD="$POSTGRES_PASSWORD" exec psql -X --set ON_ERROR_STOP=1 --username="$POSTGRES_USER" --dbname="$POSTGRES_DB"'

schema_present=$(
  "${compose[@]}" exec -T restore-db sh -ec 'PGPASSWORD="$POSTGRES_PASSWORD" exec psql -XAtq --username="$POSTGRES_USER" --dbname="$SOURCE_APP_DB_NAME"' <<'SQL'
SELECT to_regclass('public.seaql_migrations') IS NOT NULL;
SQL
)
[[ "$schema_present" == t ]] || { printf '%s\n' 'Restored cluster lacks the expected application migration table.' >&2; exit 1; }
printf 'Restore completed in isolated project %s; no app, WAHA, or consumer was started.\n' "$project"
