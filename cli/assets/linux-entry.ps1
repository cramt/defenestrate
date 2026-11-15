# Finds the firmware's Linux boot entry. Dot-sourced by reboot-to-linux.ps1
# (BootNext, once) and linux-first.ps1 (first in BootOrder, every startup),
# which differ in how many matches they accept, so this returns them all.
#
# Which entry is Linux comes from reboot.json (`linuxBootEntry`):
# {"description": null} matches every entry, in the firmware's boot order,
# that boots an .efi file other than Windows Boot Manager or the
# removable-media fallback; a description matches the entries with exactly
# that description.

# Well-known BCD object IDs and element types (BCD WMI provider docs).
$fwbootmgr = '{a5a30fa2-3d06-4e9f-b5f4-a01df9d1fcba}'
$bootmgr = '{9dea862c-5cdd-4e70-acc1-f32b344d4795}'
$DisplayOrder = 0x24000001
$AppPath = 0x12000002
$Description = 0x12000004

# Through WMI rather than parsing `bcdedit /enum`, whose labels are
# translated on non-English installs. Objects are opened by WMI path:
# BcdStore.OpenObject hands back an embedded object, which PowerShell gives
# no methods, so GetElement couldn't be called on it.
function Open([string]$id) {
    [wmi]"root\wmi:BcdObject.Id=`"$id`",StoreFilePath=`"`""
}
function Element($object, [uint32]$type) {
    try {
        $r = $object.GetElement($type)
        if ($r.ReturnValue) { return $r.Element }
    } catch {
        # A missing element (no path on a firmware-UI entry, say) throws,
        # with an empty message, rather than returning ReturnValue false.
    }
    return $null
}

# The firmware's entries in BootOrder order.
function Get-FirmwareEntries {
    $order = (Element (Open $fwbootmgr) $DisplayOrder).Ids
    if (-not $order) { throw "no displayorder in {fwbootmgr}; is this a UEFI boot?" }
    foreach ($id in $order) {
        $o = Open $id
        [pscustomobject]@{
            Id = $id
            Description = "$((Element $o $Description).String)"
            Path = "$((Element $o $AppPath).String)"
        }
    }
}

# @{ Want = what was looked for, for the log; Pinned = whether by
# description; Matches = the entries, in boot order }.
function Find-LinuxEntries($entries) {
    $config = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'reboot.json') | ConvertFrom-Json
    if ($config.description) {
        $want = "description '$($config.description)'"
        $hits = $entries | Where-Object { $_.Description -ceq $config.description }
    } else {
        $want = 'auto: entries booting an .efi file that is not Windows or the removable-media fallback'
        # Shell, network (PXE/HTTP) and setup-menu entries have no file path.
        # A USB stick's entry, if the firmware made one, boots the fallback
        # path, and so does a disk's "UEFI OS" entry; a Linux install that
        # only boots that way needs linuxBootEntry set.
        $hits = $entries | Where-Object {
            $_.Id -ne $bootmgr -and
            $_.Path -like '*.efi' -and
            $_.Path -notlike '\EFI\Microsoft\*' -and
            $_.Path -ne '\EFI\BOOT\BOOTX64.EFI'
        }
    }
    @{ Want = $want; Pinned = [bool]$config.description; Matches = @($hits) }
}
