# Teza local development on Windows

Keep Teza development caches, temporary files, and build outputs on drive J. Use the workspace for frontend output and `J:\litechat\target` for Rust and Tauri output. Put temporary checkouts, archives, test fixtures, and tool caches under `J:\litechat\build\local`. Launch Teza tooling with the following process-local PowerShell configuration, including when working from a temporary checkout.

```powershell
$tezaLocalRoot = 'J:\litechat\build\local'
$tezaToolDirectories = @{
    TEMP = Join-Path $tezaLocalRoot 'temp'
    TMP = Join-Path $tezaLocalRoot 'temp'
    CARGO_HOME = Join-Path $tezaLocalRoot 'cargo'
    CARGO_TARGET_DIR = 'J:\litechat\target'
    npm_config_cache = Join-Path $tezaLocalRoot 'npm-cache'
    npm_config_store_dir = 'J:\litechat\.pnpm-store'
    npm_config_cache_dir = Join-Path $tezaLocalRoot 'pnpm-cache'
    npm_config_state_dir = Join-Path $tezaLocalRoot 'pnpm-state'
}
foreach ($tezaSetting in $tezaToolDirectories.GetEnumerator()) {
    New-Item -ItemType Directory -Path $tezaSetting.Value -Force -ErrorAction Stop | Out-Null
    [Environment]::SetEnvironmentVariable($tezaSetting.Key, $tezaSetting.Value, 'Process')
}
```

Apply this configuration in the same shell that runs Cargo, pnpm, Node, or Tauri so child processes inherit it. Use a separate directory under the build output path for temporary builds when isolation is needed. Keep these settings scoped to Teza commands.
