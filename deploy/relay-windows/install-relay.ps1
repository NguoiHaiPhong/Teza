param(
    [string]$ListenIp = "0.0.0.0",
    [int]$Port = 1088,
    [string]$Username = "litechat",
    [string]$Password = "",
    [switch]$AllowAll,
    [string]$AllowedSubnet = "Any",
    [string]$BinaryPath = ""
)

$ErrorActionPreference = "Stop"

# Verify Administrator privilege
$principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Error "Vui long mo PowerShell voi quyen Administrator de cai dat LiteChat Relay."
    exit 1
}

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
if ([string]::IsNullOrWhiteSpace($BinaryPath)) {
    $candidates = @(
        (Join-Path $scriptDir "litechat-relay.exe"),
        (Join-Path $scriptDir "..\..\target\release\litechat-relay.exe")
    )
    $BinaryPath = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
}

if (-not $BinaryPath -or -not (Test-Path $BinaryPath)) {
    Write-Error "Khong tim thay litechat-relay.exe. Hay dat file litechat-relay.exe cung thu muc voi install-relay.ps1 hoac truyen -BinaryPath."
    exit 1
}

if ([string]::IsNullOrWhiteSpace($Password)) {
    $chars = "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789"
    $rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
    $bytes = New-Object byte[] 20
    $rng.GetBytes($bytes)
    $Password = -join ($bytes | ForEach-Object { $chars[$_ % $chars.Length] })
}

$installDir = "C:\ProgramData\LiteChatRelay"
$targetExe = Join-Path $installDir "litechat-relay.exe"
$configPath = Join-Path $installDir "config.json"
$taskName = "LiteChatRelay"
$fwRuleName = "LiteChat Relay Proxy (TCP $Port)"

if (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue) {
    Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500
}

New-Item -ItemType Directory -Path $installDir -Force | Out-Null
Copy-Item -Path $BinaryPath -Destination $targetExe -Force

$cfg = [ordered]@{
    listen = "${ListenIp}:${Port}"
    username = $Username
    password = $Password
    allow_all = [bool]$AllowAll
    extra_allowed_hosts = @()
    allow_private_targets = $false
    verbose = $false
}
$json = $cfg | ConvertTo-Json -Depth 3
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)
[System.IO.File]::WriteAllText($configPath, $json, $utf8NoBom)

# Lock permissions to SYSTEM and Builtin Administrators only
icacls $installDir /inheritance:r /grant:r "*S-1-5-18:(OI)(CI)F" "*S-1-5-32-544:(OI)(CI)F" | Out-Null

# Configure Windows Firewall rule
Get-NetFirewallRule -DisplayName "LiteChat Relay Proxy*" -ErrorAction SilentlyContinue | Remove-NetFirewallRule -ErrorAction SilentlyContinue
if ($AllowedSubnet -eq "Any") {
    New-NetFirewallRule -DisplayName $fwRuleName -Direction Inbound -Protocol TCP -LocalPort $Port -Action Allow -Profile Private,Domain,Public | Out-Null
} else {
    New-NetFirewallRule -DisplayName $fwRuleName -Direction Inbound -Protocol TCP -LocalPort $Port -RemoteAddress $AllowedSubnet -Action Allow -Profile Private,Domain,Public | Out-Null
}

# Register Scheduled Task running silently as SYSTEM at boot
$action = New-ScheduledTaskAction -Execute $targetExe -Argument "--config `"$configPath`"" -WorkingDirectory $installDir
$trigger = New-ScheduledTaskTrigger -AtStartup
$settings = New-ScheduledTaskSettingsSet `
    -AllowStartIfOnBatteries `
    -DontStopIfGoingOnBatteries `
    -ExecutionTimeLimit (New-TimeSpan -Days 0) `
    -RestartCount 999 `
    -RestartInterval (New-TimeSpan -Minutes 1) `
    -MultipleInstances IgnoreNew
$taskPrincipal = New-ScheduledTaskPrincipal -UserId "NT AUTHORITY\SYSTEM" -LogonType ServiceAccount -RunLevel Highest

Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger -Settings $settings -Principal $taskPrincipal -Force | Out-Null
Start-ScheduledTask -TaskName $taskName
Start-Sleep -Seconds 1

$listening = Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue
$lanIps = (Get-NetIPAddress -AddressFamily IPv4 -ErrorAction SilentlyContinue | Where-Object { $_.IPAddress -ne "127.0.0.1" -and $_.PrefixOrigin -ne "WellKnown" } | Select-Object -ExpandProperty IPAddress) -join ", "

Write-Host ""
Write-Host "=====================================================" -ForegroundColor Green
Write-Host " CAI DAT LITECHAT RELAY THANH CONG" -ForegroundColor Green
Write-Host "=====================================================" -ForegroundColor Green
Write-Host " Trang thai cong $Port : $(if ($listening) { 'DANG LANG NGHE' } else { 'CHO KHOI DONG' })"
Write-Host " IP may chu (LAN)      : $lanIps"
Write-Host " Giao thuc ho tro      : SOCKS5h va HTTP CONNECT (chung 1 cong $Port)"
Write-Host " Tai khoan             : $Username"
Write-Host " Mat khau              : $Password"
Write-Host " Che do loc ten mien   : $(if ($AllowAll) { 'ALLOW_ALL (tu do)' } else { 'ALLOWLIST (chi LiteChat + AI dich + GitHub update)' })"
Write-Host " Thu muc cai dat       : $installDir"
Write-Host "=====================================================" -ForegroundColor Green
