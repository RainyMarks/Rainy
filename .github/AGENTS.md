# AGENTS.md — GitHub Actions

`workflows/rainy.yml` is the only workflow. On pushes to `main`, pull requests and manual dispatch it runs on `ubuntu-24.04` and `windows-2022` with Node 24: `pnpm install --frozen-lockfile`, `pnpm run typecheck`, `pnpm exec vitest run`, `pnpm run test:node` and `pnpm run build`. Installers are built and signed on a Windows workstation with `scripts/package.ps1` ([desktop guide](../docs/desktop.md)); no workflow publishes releases.
