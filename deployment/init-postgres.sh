#!/bin/sh
set -eu

fail() {
  printf '%s\n' "$1" >&2
  exit 1
}

identifier() {
  case "$2" in
    ''|*[!A-Za-z0-9_]*) fail "$1 must contain only ASCII letters, digits, and underscore" ;;
  esac
}

uri_password() {
  case "$2" in
    ''|*[!A-Za-z0-9_-]*) fail "$1 must be URL-safe ASCII; generate with openssl rand -hex 32" ;;
  esac
  [ "${#2}" -ge 32 ] || fail "$1 must be at least 32 characters"
}

identifier APP_DB_NAME "${APP_DB_NAME:?APP_DB_NAME is required}"
identifier APP_DB_USER "${APP_DB_USER:?APP_DB_USER is required}"
identifier WAHA_DB_USER "${WAHA_DB_USER:?WAHA_DB_USER is required}"
identifier POSTGRES_USER "${POSTGRES_USER:?POSTGRES_USER is required}"
uri_password POSTGRES_ADMIN_PASSWORD "${POSTGRES_PASSWORD:?POSTGRES_ADMIN_PASSWORD is required}"
uri_password APP_DB_PASSWORD "${APP_DB_PASSWORD:?APP_DB_PASSWORD is required}"
uri_password WAHA_DB_PASSWORD "${WAHA_DB_PASSWORD:?WAHA_DB_PASSWORD is required}"
[ "$APP_DB_NAME" != postgres ] && [ "$APP_DB_NAME" != template0 ] && [ "$APP_DB_NAME" != template1 ] || fail "APP_DB_NAME must be a dedicated database"
[ "$APP_DB_USER" != "$WAHA_DB_USER" ] || fail "APP_DB_USER and WAHA_DB_USER must be different"
[ "$APP_DB_USER" != "$POSTGRES_USER" ] && [ "$WAHA_DB_USER" != "$POSTGRES_USER" ] && [ "$APP_DB_USER" != postgres ] && [ "$WAHA_DB_USER" != postgres ] || fail "application and WAHA roles must be separate from database administrators"
[ "$APP_DB_PASSWORD" != "$WAHA_DB_PASSWORD" ] && [ "$APP_DB_PASSWORD" != "$POSTGRES_PASSWORD" ] && [ "$WAHA_DB_PASSWORD" != "$POSTGRES_PASSWORD" ] || fail "database role passwords must be distinct"
psql --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" --set ON_ERROR_STOP=1 <<'SQL'
\getenv app_db_name APP_DB_NAME
\getenv app_db_user APP_DB_USER
\getenv app_db_password APP_DB_PASSWORD
\getenv waha_db_user WAHA_DB_USER
\getenv waha_db_password WAHA_DB_PASSWORD

SELECT format('CREATE ROLE %I LOGIN PASSWORD %L', :'app_db_user', :'app_db_password') \gexec
SELECT format('CREATE DATABASE %I OWNER %I', :'app_db_name', :'app_db_user') \gexec
SELECT format('CREATE ROLE %I LOGIN CREATEDB PASSWORD %L', :'waha_db_user', :'waha_db_password') \gexec
SELECT format('REVOKE CONNECT ON DATABASE %I FROM PUBLIC', :'app_db_name') \gexec
SELECT format('GRANT CONNECT ON DATABASE %I TO %I', :'app_db_name', :'app_db_user') \gexec
REVOKE CONNECT ON DATABASE postgres FROM PUBLIC;
GRANT CONNECT, CREATE ON DATABASE postgres TO :"waha_db_user";
GRANT CREATE ON SCHEMA public TO :"waha_db_user";
SQL
