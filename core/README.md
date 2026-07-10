# Valv Core

`@valv/core` is the self-hostable Node.js backend for Valv. It exposes auth, device registration, metadata (op log, delta pull, folders, invites), blob batch, and realtime sync APIs over HTTP and WebSocket.

## Prerequisites

- Node.js LTS
- pnpm
- An S3-compatible bucket, such as Cloudflare R2, MinIO, or Backblaze B2
- Optional: an SMTP server for invite emails, such as any SMTP provider

## First-Time Setup

Run these commands from `core/`:

```bash
pnpm install
cp .env.example .env
pnpm db:migrate:sqlite
```

After copying `.env.example`, fill in `VALV_AUTH_SECRET` and your S3-compatible bucket credentials. Leave `SMTP_PASS`/`EMAIL_FROM` unset to run with invite emails disabled.

`db:migrate:sqlite` runs `drizzle-kit migrate` against `./src/db/migrations/sqlite` using `VALV_DATABASE_URL`. PostgreSQL is also supported by the schema and `drizzle.pg.config.ts`, but there is no dedicated `db:migrate:pg` package script; run `pnpm exec drizzle-kit migrate --config drizzle.pg.config.ts` directly against a Postgres `VALV_DATABASE_URL` instead.

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

The second command returns `{ "device_id": "...", "token": "..." }`. Store those values with `valv auth login` (preferred) or by writing `~/.config/valv/config.toml` directly, as described in [`../crates/README.md`](../crates/README.md).

## Self-Hosting Limitations

This repository provides the backend API and the CLI/daemon/app clients that talk to it. It does not include:

- A Docker Compose stack or a production container image.
- A web UI for accepting invites. `POST /folders/:id/invites` and `valv grant create --to <email>` produce an invite URL of the form `{backend_url}/invites/{token}/accept`; that URL is a backend API endpoint that must be called with `POST` (for example from a script or your own frontend), not a page a browser can open directly. Device grants created with `valv grant create --device <name>` do not have this limitation: they print a device token directly.

## Tests

```bash
pnpm test
```

Runs the core unit test suite (Vitest) against an in-memory SQLite database with no external services.

```bash
pnpm --filter @valv/e2e test
```

Runs the `@valv/e2e` API suite. It boots core against an in-memory SQLite database but requires a real S3-compatible endpoint reachable at `http://localhost:9000` with `minioadmin`/`minioadmin` credentials (for example a local MinIO server) to exercise the blobstore routes.

```bash
../e2e/smoke/run-all.sh
```

Runs the numbered daemon/CLI smoke scenarios. These build and exercise `valvd` and `valv` against core, and additionally require MinIO plus the MinIO Client (`mc`) on `PATH`.

## Environment Variables

| Name | Required | Default | Description |
| --- | --- | --- | --- |
| `VALV_DATABASE_URL` | Yes | None | Database URL. Use `file:./dev.db` for local SQLite, or a `postgres://...` URL for PostgreSQL. |
| `VALV_AUTH_SECRET` | Yes | None | Better Auth secret. Generate with `openssl rand -hex 32`. |
| `VALV_BASE_URL` | No | `http://localhost:${VALV_PORT}` | Public base URL for auth and invite links. |
| `VALV_MIN_PROTOCOL` | No | None (no minimum enforced) | Minimum sync protocol version metadata requests must present via the `X-Valv-Protocol` header; requests below it are rejected as requiring an update. |
| `VALV_PORT` | No | `4747` | HTTP server port. |
| `BUCKET_ENDPOINT` | Yes | None | S3-compatible bucket endpoint. |
| `BUCKET_NAME` | Yes | None | Bucket name. |
| `BUCKET_ACCESS_KEY_ID` | Yes | None | Bucket access key. |
| `BUCKET_SECRET_ACCESS_KEY` | Yes | None | Bucket secret key. |
| `SMTP_HOST` | No | `smtp.mx.cloudflare.net` | SMTP host for invite emails. |
| `SMTP_PORT` | No | `465` | SMTP port. Non-465 ports use STARTTLS. |
| `SMTP_USER` | No | `apitoken` | SMTP auth username. |
| `SMTP_PASS` | No | None | SMTP password or API token. Required with `EMAIL_FROM` to enable invite emails. |
| `EMAIL_FROM` | No | None | Verified sender address. Required with `SMTP_PASS` to enable invite emails. |
