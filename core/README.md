# Valv Core

`@valv/core` is the self-hostable Node.js backend for Valv. It exposes auth, device registration, metadata, blob batch, and realtime sync APIs.

## Prerequisites

- Node.js LTS
- pnpm
- An S3-compatible bucket, such as Cloudflare R2, MinIO, or Backblaze B2
- Optional: an SMTP server for invite emails, such as any SMTP provider or Mailpit for local testing

## First-Time Setup

Run these commands from `core/`:

```bash
pnpm install
cp .env.example .env
pnpm db:migrate:sqlite
```

After copying `.env.example`, fill in credentials for your S3-compatible bucket. Leave the SMTP vars commented out to run with invite emails disabled.

## Migration Runbook

After applying the migration that creates `version_chunks`, run the one-time reverse-index backfill before deploying or enabling any build that authorizes downloads from `version_chunks`:

```bash
pnpm backfill:version-chunks
```

The command reads `VALV_DATABASE_URL` or `DATABASE_URL`, pages through existing `versions` rows, and is safe to rerun. Do not cut over chunk-download authorization to the scoped `version_chunks` query in an environment until this backfill has completed there.

## Running The Server

```bash
pnpm dev
```

The server listens on `VALV_PORT`, which defaults to `4747`, and logs `valv core listening on ...` when ready.

## Device Registration

Register a user, save the session cookie, then register the local device.

```bash
curl -i -c /tmp/valv-cookies.txt \
  -H 'content-type: application/json' \
  -d '{"name":"Local Dev","email":"dev@example.com","password":"replace-with-password"}' \
  http://localhost:4747/api/auth/sign-up/email
```

```bash
curl -s -b /tmp/valv-cookies.txt \
  -H 'content-type: application/json' \
  -d '{"name":"Dev Mac"}' \
  http://localhost:4747/auth/device
```

The second command returns `{ "device_id": "...", "token": "..." }`. Write those values to `~/.config/valv/config.toml` using the format shown in `../crates/README.md`.

## Tests

```bash
pnpm test
```

Runs unit tests in memory with no external services.

```bash
pnpm smoke
```

Runs SQLite integration smoke tests.

## Environment Variables

| Name | Required | Default | Description |
| --- | --- | --- | --- |
| `VALV_DATABASE_URL` | Yes | None | Database URL. Use `file:./dev.db` for local SQLite. |
| `VALV_AUTH_SECRET` | Yes | None | Better Auth secret. Generate with `openssl rand -hex 32`. |
| `VALV_BASE_URL` | No | `http://localhost:${VALV_PORT}` | Public base URL for auth and invite links. |
| `BUCKET_ENDPOINT` | Yes | None | S3-compatible bucket endpoint. |
| `BUCKET_NAME` | Yes | None | Bucket name. |
| `BUCKET_ACCESS_KEY_ID` | Yes | None | Bucket access key. |
| `BUCKET_SECRET_ACCESS_KEY` | Yes | None | Bucket secret key. |
| `VALV_PORT` | No | `4747` | HTTP server port. |
| `SMTP_HOST` | No | `smtp.mx.cloudflare.net` | SMTP host for invite emails. |
| `SMTP_PORT` | No | `465` | SMTP port. Non-465 ports use STARTTLS. |
| `SMTP_USER` | No | `apitoken` | SMTP auth username. |
| `SMTP_PASS` | No | None | SMTP password or API token. Required with `EMAIL_FROM` to enable invite emails. |
| `EMAIL_FROM` | No | None | Verified sender address. Required with `SMTP_PASS` to enable invite emails. |
