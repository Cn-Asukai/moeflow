# Frontend/API contract regression checks

These checks exercise the real Rust API, MongoDB transactions, CAPTCHA challenges,
sessions, target creation, invitation/application processing, unread counts,
verification cooldowns, password boundaries, and missing group IDs.

Use an isolated local Compose project. The test creates accounts and teams; do not
point it at a database used by people. It reads bootstrap credentials from
`backend-rust/.env`, without printing them. Cloud credentials are unnecessary.

From the repository root, after building the regular local images:

```powershell
docker tag moeflow-backend:latest moeflow-contract-tests-backend:latest
docker tag moeflow-frontend:latest moeflow-contract-tests-frontend:latest
$env:HTTP_PORT = '8081'
$env:MAIL_UI_PORT = '8026'
docker compose -p moeflow-contract-tests --env-file backend-rust/.env -f backend-rust/compose.yaml up -d --no-build --wait
$env:MOEFLOW_TEST_URL = 'http://127.0.0.1:8081'
node backend-rust/tests/frontend_contracts.cjs
docker compose -p moeflow-contract-tests --env-file backend-rust/.env -f backend-rust/compose.yaml down -v
Remove-Item Env:HTTP_PORT, Env:MAIL_UI_PORT, Env:MOEFLOW_TEST_URL
```

Frontend password boundary checks, from `frontend-v1`:

```powershell
node node_modules/tsx/dist/cli.mjs --test scripts/password.test.ts
```
