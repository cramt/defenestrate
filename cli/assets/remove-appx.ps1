# Runs from specialize.cmd. A provisioned package installs itself into every
# new account at first sign-in; at specialize there are no accounts yet, so
# deprovisioning here means nobody ever has these.
$wanted = Get-Content "$PSScriptRoot\appx-remove.txt" | Where-Object { $_ }
$provisioned = Get-AppxProvisionedPackage -Online
$failed = $false

foreach ($name in $wanted) {
    $packages = @($provisioned | Where-Object DisplayName -eq $name)
    # The build only checks that the image ships the package. Frameworks and
    # stubs ship without being provisioned, so there's nothing to remove.
    if ($packages.Count -eq 0) {
        Write-Output "not provisioned: $name"
        continue
    }
    foreach ($package in $packages) {
        try {
            Remove-AppxProvisionedPackage -Online -PackageName $package.PackageName -ErrorAction Stop | Out-Null
            Write-Output "removed: $($package.PackageName)"
        } catch {
            Write-Output "failed: $($package.PackageName): $_"
            $failed = $true
        }
    }
}

if ($failed) { exit 1 }
