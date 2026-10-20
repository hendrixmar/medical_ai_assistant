#!/usr/bin/env bash
set -Eeuo pipefail
umask 077

# Invoke nightly from the operator's scheduler. This script never chooses or prunes retention.
: "${COMPOSE_ENV_FILE:?Set COMPOSE_ENV_FILE to the owner-only production Compose env file}"
: "${BACKUP_SSH_TARGET:?Set BACKUP_SSH_TARGET to the operator-provisioned user@host}"
: "${BACKUP_REMOTE_DIR:?Set BACKUP_REMOTE_DIR to a pre-created remote directory}"
: "${AGE_RECIPIENT:?Set AGE_RECIPIENT to the public age recipient; keep its identity off this VPS}"
: "${OFFSITE_BACKUP_ACK:?Set OFFSITE_BACKUP_ACK=true after verifying the SSH target is a separate off-server system}"

[[ "$OFFSITE_BACKUP_ACK" == true ]] || { printf '%s\n' 'Refusing backup without explicit off-server confirmation.' >&2; exit 2; }
[[ -f "$COMPOSE_ENV_FILE" && ! -L "$COMPOSE_ENV_FILE" ]] || { printf '%s\n' 'Compose env file must be a regular non-symlink file.' >&2; exit 2; }
[[ "$COMPOSE_ENV_FILE" == /* ]] || { printf '%s\n' 'COMPOSE_ENV_FILE must be an absolute path.' >&2; exit 2; }
mode=$(stat -c '%a' -- "$COMPOSE_ENV_FILE")
[[ "$mode" == 600 || "$mode" == 400 ]] || { printf '%s\n' 'Compose env file must be mode 0600 or 0400.' >&2; exit 2; }
[[ "$BACKUP_SSH_TARGET" =~ ^[A-Za-z0-9._-]+@[A-Za-z0-9.-]+$ ]] || { printf '%s\n' 'BACKUP_SSH_TARGET must be user@host without a path or port.' >&2; exit 2; }
[[ "$BACKUP_REMOTE_DIR" =~ ^(/[A-Za-z0-9_-]+)+$ ]] || { printf '%s\n' 'BACKUP_REMOTE_DIR must be an absolute path using only letters, digits, underscore, and dash.' >&2; exit 2; }

project=${COMPOSE_PROJECT_NAME:-medical-assistant-vps}
[[ "$project" =~ ^[a-z0-9][a-z0-9_-]*$ ]] || { printf '%s\n' 'Invalid COMPOSE_PROJECT_NAME.' >&2; exit 2; }
command -v age >/dev/null
command -v docker >/dev/null
command -v sftp >/dev/null
command -v sha256sum >/dev/null

script_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
compose_file="$script_dir/compose.yml"
workdir=$(mktemp -d /tmp/medical-assistant-backup.XXXXXXXX)
trap 'rm -rf -- "$workdir"' EXIT
stamp=$(date -u +%Y%m%dT%H%M%SZ)
name="medical-assistant-${stamp}-$$.dump.age"
archive="$workdir/$name"
checksum="$archive.sha256"

# Include the postgres database: WAHA's default GOWS schemas and media live there.
# This full-cluster dump also includes roles but deliberately omits their password hashes.
docker compose \
  --env-file "$COMPOSE_ENV_FILE" \
  --project-name "$project" \
  --file "$compose_file" \
  exec -T db sh -ec 'PGPASSWORD="$POSTGRES_PASSWORD" exec pg_dumpall --no-role-passwords --username="$POSTGRES_USER"' \
  | age --encrypt --recipient "$AGE_RECIPIENT" > "$archive"

(
  cd -- "$workdir"
  sha256sum -- "$name" > "$name.sha256"
)
remote="$BACKUP_REMOTE_DIR/$name"
remote_checksum="$remote.sha256"
remote_partial="$remote.partial-$$"
remote_checksum_partial="$remote_checksum.partial-$$"

# Strict host-key checking and batch mode require operator-provisioned SSH trust/keys.
sftp -oBatchMode=yes -oStrictHostKeyChecking=yes -b - "$BACKUP_SSH_TARGET" <<SFTP
put $archive $remote_partial
put $checksum $remote_checksum_partial
rename $remote_partial $remote
rename $remote_checksum_partial $remote_checksum
SFTP

printf '%s\n' 'Encrypted Postgres dump and checksum transferred to the configured off-server target.'
