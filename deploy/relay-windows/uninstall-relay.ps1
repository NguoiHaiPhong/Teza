$ErrorActionPreference = "Stop"

$principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Error "Vui long mo PowerShell voi quyen Administrator de go cai dat."
    exit 1
}

$taskName = "LiteChatRelay"
$installDir = "C:\ProgramData\LiteChatRelay"

if (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue) {
    Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
    Write-Host "Da xoa Scheduled Task: $taskName"
}

Get-NetFirewallRule -DisplayName "LiteChat Relay Proxy*" -ErrorAction SilentlyContinue | Remove-NetFirewallRule -ErrorAction SilentlyContinue
Write-Host "Da xoa quy tac Windows Firewall."

if (Test-Path $installDir) {
    Start-Sleep -Milliseconds 500
    Remove-Item -Path $installDir -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host "Da xoa thu muc: $installDir"
}

Write-Host "Hoan tat go cai dat LiteChat Relay."
