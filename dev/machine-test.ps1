# Starts agentsignals.exe the way a new user's machine would, and checks it never fails silently:
#   1. a fresh user profile (no Claude Code sessions, no settings yet): the window opens, the API answers;
#   2. a second start: it hands over to the running one and exits;
#   3. another program on port 47801: an error message, never a hidden process.
# Then the unit tests and the live Windows Terminal test (one tab opens and closes by itself).
# Agent Signals must not be running. Usage: dev\machine-test.ps1 [path to agentsignals.exe]
param([string]$Exe = "$PSScriptRoot\..\target\release\agentsignals.exe")
$ErrorActionPreference = 'Stop'
$Exe = (Resolve-Path $Exe).Path
if (Get-NetTCPConnection -LocalPort 47801 -State Listen -ErrorAction SilentlyContinue) { throw 'Port 47801 is in use: close Agent Signals first.' }
Add-Type -Namespace T -Name U -MemberDefinition @'
[DllImport("user32.dll")] public static extern int GetClassName(IntPtr h, System.Text.StringBuilder s, int n);
'@
function ClassOf($proc) { $proc.Refresh(); $b = New-Object Text.StringBuilder 64; [void][T.U]::GetClassName($proc.MainWindowHandle, $b, 64); $b.ToString() }
$failed = 0
function Check($ok, $what) { if ($ok) { "PASS  $what" } else { "FAIL  $what"; $script:failed++ } }

# A fresh profile in a temp folder; Claude markers removed (a session started from Claude carries them).
$fresh = Join-Path ([IO.Path]::GetTempPath()) "agentsignals-machine-test-$PID"
New-Item -ItemType Directory -Force "$fresh\AppData\Roaming" | Out-Null
$saved = @{ USERPROFILE = $env:USERPROFILE; APPDATA = $env:APPDATA }
Get-ChildItem env: | Where-Object Name -like 'CLAUDE*' | ForEach-Object { [Environment]::SetEnvironmentVariable($_.Name, $null) }
$env:USERPROFILE = $fresh; $env:APPDATA = "$fresh\AppData\Roaming"
try {
    $first = Start-Process $Exe -WorkingDirectory $fresh -PassThru; Start-Sleep 4
    $api = try { Invoke-RestMethod http://127.0.0.1:47801/sessions -TimeoutSec 3; $true } catch { $false }
    Check (!$first.HasExited -and $first.MainWindowHandle -ne 0 -and $api) 'fresh profile: window open, API answers'
    Check (Test-Path "$env:APPDATA\AgentSignals\sessions.json") 'fresh profile: settings folder created'

    $second = Start-Process $Exe -WorkingDirectory $fresh -PassThru; Start-Sleep 3
    Check ($second.HasExited -and !$first.HasExited) 'second start: exits, the first keeps running'
    Stop-Process -Id $first.Id -Force; Start-Sleep -Milliseconds 500

    $other = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 47801); $other.Start()
    try {
        # Watched for, not expected to stay: the box takes the keyboard, and a key pressed meanwhile closes it.
        $blocked = Start-Process $Exe -WorkingDirectory $fresh -PassThru; $box = $false
        foreach ($i in 1..40) { Start-Sleep -Milliseconds 250; if (!$blocked.HasExited -and (ClassOf $blocked) -eq '#32770') { $box = $true; break } }
        Check $box 'port taken by another program: error message shown'
        if (!$blocked.HasExited) { Stop-Process -Id $blocked.Id -Force }
    } finally { $other.Stop() }

    Check (!(Test-Path "$env:APPDATA\AgentSignals\crash.log")) 'no crash.log'
} finally {
    $env:USERPROFILE = $saved.USERPROFILE; $env:APPDATA = $saved.APPDATA
    Remove-Item $fresh -Recurse -Force -ErrorAction SilentlyContinue
}

Push-Location "$PSScriptRoot\.."
try {
    cargo test --release -- --include-ignored 2>&1 | Select-String -Pattern '^test |test result' | Out-Host
    Check ($LASTEXITCODE -eq 0) 'unit tests and the live Windows Terminal test'
} finally { Pop-Location }
if ($failed) { throw "$failed check(s) failed" }
'All checks passed.'
