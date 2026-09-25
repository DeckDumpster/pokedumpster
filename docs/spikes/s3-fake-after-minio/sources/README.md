# Sources for spike db-hny6

Every page cited in `../RESULT.md`, preserved verbatim on 2026-09-25, because a
citation that cannot be re-read is not a citation. URLs 403, rotate, and return
a 404 page under HTTP 200 — and this spike is *about* a project whose entire
web presence was withdrawn, so keeping the evidence on someone else's server
would be a particularly poor joke.

HTML was reduced to readable text (script/style stripped, tags removed,
entities unescaped); the `.json` files are GitHub / Docker Hub API responses
kept raw. Every file carries a two-line provenance header: the source URL and
the retrieval date.

| file | what it establishes |
|---|---|
| `minio-github-archived.txt` | minio/minio archived 2026-04-25, read-only, "NO LONGER MAINTAINED" |
| `minio-dockerhub-removal-stablebuild.txt` | third-party record of the Docker Hub namespace removal and its timing |
| `silo-github.txt` | Silo is a fork of minio/minio; AGPL-3.0; the pgsty/minio → pgsty/silo rename |
| `silo-pgsty.txt` | the project portal: what the fork undertakes to keep publishing, and how artefacts are signed |
| `silo-repo-api.json` | stars, open issues, `pushed_at`, licence (GitHub API) |
| `silo-releases-api.json` | server release cadence |
| `silo-mc-github.txt`, `silo-mcli-compatibility.txt` | the client fork and its compatibility statement |
| `silo-mc-releases-api.json` | client release cadence |
| `silo-dockerhub-tags.json` | which tags exist and when they were pushed |
| `rustfs-github.txt` | Apache-2.0, feature matrix, the bounded MinIO-compatibility claim |
| `rustfs-repo-api.json`, `rustfs-releases-api.json`, `rustfs-dockerhub-tags.json` | stars, issues, release cadence, tags |
| `rustfs-advisory-iam-deny-only.txt` | GHSA-xgr5-qc6w-vcg9 / CVE-2026-22043 — the IAM deny-only short-circuit |
| `seaweedfs-github.txt` | the project's own S3 feature claims |
| `seaweedfs-releases-api.json` | release cadence |
| `seaweedfs-issue-8516-iam.txt` | IAM policies attached but not enforced (v4.14, 2026-03-05) |
| `seaweedfs-issue-8754-lifecycle.txt` | lifecycle configuration cannot be applied (v4.15) |
| `garage-s3-compatibility.txt` | Garage's own table: no versioning, no bucket policy, partial lifecycle |
| `adobe-s3mock-github.txt` | S3Mock's surface, and its static-credential test-double design |
| `localstack-iam-enforcement.txt` | Community edition does not enforce IAM policy evaluation |
| `localstack-road-ahead.txt` | from March 2026 an account is required to run it |
