#!/usr/bin/env bash
set -euo pipefail
umask 077
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ENV_FILE="$ROOT/.env"
if [[ ! -f "$ENV_FILE" ]]; then
    command -v openssl >/dev/null || { printf '%s\n' 'openssl is required to generate deployment secrets' >&2; exit 1; }
    TEMP_ENV="$(mktemp "$ROOT/.env.tmp.XXXXXX")"
    trap 'rm -f -- "$TEMP_ENV"' EXIT
    while IFS= read -r line || [[ -n "$line" ]]; do
        case "$line" in
            MONGO_PASSWORD=) printf 'MONGO_PASSWORD=%s\n' "$(openssl rand -hex 24)" ;;
            MONGO_APP_PASSWORD=) printf 'MONGO_APP_PASSWORD=%s\n' "$(openssl rand -hex 24)" ;;
            JWT_SECRET=) printf 'JWT_SECRET=%s\n' "$(openssl rand -hex 48)" ;;
            ADMIN_PASSWORD=) printf 'ADMIN_PASSWORD=%s\n' "$(openssl rand -hex 24)" ;;
            *) printf '%s\n' "$line" ;;
        esac
    done < "$ROOT/.env.example" > "$TEMP_ENV"
    mv -- "$TEMP_ENV" "$ENV_FILE"
    trap - EXIT
    printf '%s\n' 'Created .env with independent random credentials. Administrator credentials are in that file.'
fi
if [[ "${1:-}" != "--allow-unconfigured-cloud" ]]; then
    for key in COS_REGION COS_BUCKET COS_SECRET_ID COS_SECRET_KEY; do
        value=""
        while IFS='=' read -r name candidate; do
            if [[ "$name" == "$key" ]]; then value="${candidate//\"/}"; break; fi
        done < "$ENV_FILE"
        if [[ -z "$value" ]]; then
            printf 'Fill %s in %s and configure COS CORS before deployment. --allow-unconfigured-cloud is only for account/metadata testing.\n' "$key" "$ENV_FILE" >&2
            exit 1
        fi
    done
fi
docker compose --env-file "$ENV_FILE" -f "$ROOT/compose.yaml" config --quiet
docker compose --env-file "$ENV_FILE" -f "$ROOT/compose.yaml" up -d --build --wait --wait-timeout 180
printf '%s\n' 'Deployment started. See HTTP_PORT and MAIL_UI_PORT in .env for access addresses.'
printf '%s\n' 'AI credentials are server-side. Confirm database/object backup protection before enabling business physical deletion.'
