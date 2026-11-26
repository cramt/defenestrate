# Dot-sourced by boot.ps1 -Fakes in nix/tests/boot-queue.nix: the
# Windows-only calls, recorded to $env:FAKE_CALLS instead. An import of a
# file whose name contains $env:FAKE_FAIL fails, as reg.exe would.
function Record([string]$what) { Add-Content -LiteralPath $env:FAKE_CALLS -Value $what }
function Invoke-RegImport([string]$file) {
    Record "reg import $(Split-Path -Leaf $file)"
    if ($env:FAKE_FAIL -and (Split-Path -Leaf $file).Contains($env:FAKE_FAIL)) { throw 'reg import exited with 1' }
}
function Invoke-Spi([uint32]$action, [uint32]$uiParam, [object]$pvParam) {
    Record ('spi 0x{0:x} {1} {2}' -f $action, $uiParam, ($pvParam -join ','))
}
function Get-UserValue([string]$key, [string]$name) { 7 }
function Assert-AdminOnly([string]$path) { }
function Register-Tasks { foreach ($name in $tasks.Keys) { Record "register $name $($tasks[$name])" } }
