//! autounattend.xml.

use anyhow::{Context, Result};

use crate::config::{Config, Disk};
use crate::gpt::ImagePart;
use crate::tasks::{self, Window};

impl Variant {
    /// What the config asks for: the ISO for a USB stick.
    pub fn from_config(config: &Config) -> Self {
        Self { disk: config.disk, first_logon_end: FirstLogonEnd::Reboot, boot_prompt: config.boot_prompt }
    }

    /// For `build image`, whose VM sees one disk and nothing else: it's
    /// always DiskID 0, so wiping it unasked is safe whatever `disk` says
    /// (that one's for the ISO on a stick, next to the machine's other
    /// disks). The prompt goes because the disk is first in the VM's boot
    /// order, so the CD only boots while the disk is blank.
    pub fn for_image() -> Self {
        Self { disk: Disk::Wipe { disk_id: 0 }, first_logon_end: FirstLogonEnd::Poweroff, boot_prompt: false }
    }
}

/// How first logon ends once the shell is switched.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FirstLogonEnd {
    /// Log in again under the new shell.
    Reboot,
    /// For `build image`, whose VM knows setup is done when it powers off.
    Poweroff,
}

pub struct Variant {
    pub disk: Disk,
    pub first_logon_end: FirstLogonEnd,
    pub boot_prompt: bool,
}

pub fn render(config: &Config, v: &Variant) -> Result<String> {
    let x = escape;
    let setup = crate::render::SETUP_DIR.system_root().expect("under C:\\Windows");
    let component = |name: &str, body: String| {
        format!(
            "<component name=\"{name}\" processorArchitecture=\"amd64\" publicKeyToken=\"31bf3856ad364e35\" language=\"neutral\" versionScope=\"nonSxS\">\n{body}</component>\n"
        )
    };
    let locales = format!(
        "<InputLocale>{}</InputLocale>\n<SystemLocale>{lang}</SystemLocale>\n<UILanguage>{lang}</UILanguage>\n<UserLocale>{lang}</UserLocale>\n",
        x(&config.input_locale),
        lang = x(&config.language),
    );
    let password = format!("<Password>\n<Value>{}</Value>\n<PlainText>true</PlainText>\n</Password>\n", x(&config.user.password));

    // Setup creates partitions in Order, so Order is the partition number.
    let (disk_configuration, install_to) = match v.disk {
        Disk::Interactive => (String::new(), String::new()),
        Disk::Wipe { disk_id } => {
            let (mut create, mut modify) = (String::new(), String::new());
            for part in ImagePart::ALL {
                let n = part.number();
                let (kind, format) = match part {
                    ImagePart::Esp => ("<Type>EFI</Type><Size>300</Size>", "<Format>FAT32</Format><Label>EFI</Label>"),
                    ImagePart::Msr => ("<Type>MSR</Type><Size>16</Size>", ""),
                    ImagePart::Windows => ("<Type>Primary</Type><Extend>true</Extend>", "<Format>NTFS</Format><Label>Windows</Label><Letter>C</Letter>"),
                };
                create += &format!("<CreatePartition wcm:action=\"add\"><Order>{n}</Order>{kind}</CreatePartition>\n");
                modify += &format!("<ModifyPartition wcm:action=\"add\"><Order>{n}</Order><PartitionID>{n}</PartitionID>{format}</ModifyPartition>\n");
            }
            (
                format!(
                    "<DiskConfiguration>\n<Disk wcm:action=\"add\">\n<DiskID>{disk_id}</DiskID>\n<WillWipeDisk>true</WillWipeDisk>\n<CreatePartitions>\n{create}</CreatePartitions>\n<ModifyPartitions>\n{modify}</ModifyPartitions>\n</Disk>\n<WillShowUI>Never</WillShowUI>\n</DiskConfiguration>\n"
                ),
                format!(
                    "<InstallTo>\n<DiskID>{disk_id}</DiskID>\n<PartitionID>{}</PartitionID>\n</InstallTo>\n<InstallToAvailablePartition>false</InstallToAvailablePartition>\n<WillShowUI>Never</WillShowUI>\n",
                    ImagePart::Windows.number()
                ),
            )
        }
    };

    let windows_pe = component(
        "Microsoft-Windows-International-Core-WinPE",
        format!("<SetupUILanguage><UILanguage>{}</UILanguage></SetupUILanguage>\n{locales}", x(&config.language)),
    ) + &component(
        "Microsoft-Windows-Setup",
        format!(
            r#"{disk_configuration}<!-- Setup otherwise phones home for updated setup binaries and drivers mid-install. -->
<DynamicUpdate><Enable>false</Enable></DynamicUpdate>
<Diagnostics><OptIn>false</OptIn></Diagnostics>
<ImageInstall>
<OSImage>
{install_to}<!-- The build exports the chosen edition alone, so it's always image 1. -->
<InstallFrom>
<MetaData wcm:action="add"><Key>/IMAGE/INDEX</Key><Value>1</Value></MetaData>
</InstallFrom>
</OSImage>
</ImageInstall>
<UserData>
<AcceptEula>true</AcceptEula>
<ProductKey>
<Key>{key}</Key>
<WillShowUI>Never</WillShowUI>
</ProductKey>
</UserData>
"#,
            key = config.edition.generic_key(),
        ),
    );

    let specialize = component(
        "Microsoft-Windows-Shell-Setup",
        format!("<ComputerName>{}</ComputerName>\n<TimeZone>{}</TimeZone>\n", x(config.computer_name.as_str()), x(&config.time_zone)),
    ) + &component(
        "Microsoft-Windows-Deployment",
        format!(
            "<RunSynchronous>\n<RunSynchronousCommand wcm:action=\"add\">\n<Order>1</Order>\n<Path>{}</Path>\n</RunSynchronousCommand>\n</RunSynchronous>\n",
            x(&format!("cmd.exe /c \"{setup}\\specialize.cmd > {setup}\\specialize.log 2>&1\""))
        ),
    );

    let auto_logon = if config.user.auto_logon {
        format!(
            "<AutoLogon>\n<Enabled>true</Enabled>\n<Username>{}</Username>\n{password}<!-- Without a count, Windows turns autologon off after the first one. -->\n<LogonCount>9999999</LogonCount>\n</AutoLogon>\n",
            x(config.user.name.as_str())
        )
    } else {
        String::new()
    };

    let mut commands = vec![
        (format!("cmd.exe /c \"reg import {setup}\\shell.reg > {setup}\\firstlogon.log 2>&1\""), "Replace explorer.exe as the shell"),
        (format!("cmd.exe /c \"{setup}\\wsl\\wsl-install.cmd >> {setup}\\firstlogon.log 2>&1\""), "Install WSL"),
    ];
    if crate::apps::needed(config) {
        commands.push((
            format!("cmd.exe /c \"schtasks /create /tn {} /xml {setup}\\apps\\task.xml >> {setup}\\firstlogon.log 2>&1\"", crate::apps::TASK),
            "Install the apps at the next logon",
        ));
    }
    if crate::reboot_to_linux::enabled(config) {
        commands.push((
            {
                let (powershell, args) = tasks::powershell(&format!("{setup}\\reboot\\register.ps1"), Window::Console);
                format!("cmd.exe /c \"{powershell} {args} -Task {} >> {setup}\\firstlogon.log 2>&1\"", crate::reboot_to_linux::TASK)
            },
            "Register Reboot to Linux",
        ));
        commands.push((
            format!("cmd.exe /c \"schtasks /create /tn {} /xml {setup}\\reboot\\linux-first.xml >> {setup}\\firstlogon.log 2>&1\"", crate::reboot_to_linux::LINUX_FIRST_TASK),
            "Keep Linux first in the boot order",
        ));
    }
    commands.push(match v.first_logon_end {
        FirstLogonEnd::Reboot => ("shutdown.exe /r /t 0".into(), "Log in again under the new shell"),
        FirstLogonEnd::Poweroff => ("shutdown.exe /s /t 0".into(), "Done; the next logon gets the new shell"),
    });
    let first_logon: String = commands
        .iter()
        .enumerate()
        .map(|(i, (cmd, desc))| {
            format!(
                "<SynchronousCommand wcm:action=\"add\">\n<Order>{}</Order>\n<CommandLine>{}</CommandLine>\n<Description>{}</Description>\n</SynchronousCommand>\n",
                i + 1,
                x(cmd),
                x(desc)
            )
        })
        .collect();

    let oobe = component("Microsoft-Windows-International-Core", locales.clone())
        + &component(
            "Microsoft-Windows-Shell-Setup",
            format!(
                r#"{auto_logon}<!-- Runs elevated in the first real user session, after OOBE is done.
     That session still gets explorer; the reboot makes the next one ours. -->
<FirstLogonCommands>
{first_logon}</FirstLogonCommands>
<OOBE>
<HideEULAPage>true</HideEULAPage>
<HideOEMRegistrationScreen>true</HideOEMRegistrationScreen>
<HideOnlineAccountScreens>true</HideOnlineAccountScreens>
<HideWirelessSetupInOOBE>true</HideWirelessSetupInOOBE>
<ProtectYourPC>3</ProtectYourPC>
</OOBE>
<UserAccounts>
<LocalAccounts>
<LocalAccount wcm:action="add">
<Name>{name}</Name>
<Group>Administrators</Group>
{password}</LocalAccount>
</LocalAccounts>
</UserAccounts>
"#,
                name = x(config.user.name.as_str()),
            ),
        );

    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<unattend xmlns="urn:schemas-microsoft-com:unattend" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State">
<settings pass="windowsPE">
{windows_pe}</settings>
<settings pass="specialize">
{specialize}</settings>
<settings pass="oobeSystem">
{oobe}</settings>
</unattend>
"#
    );
    // Setup stops on a malformed answer file before it writes anything,
    // which a headless build only notices hours later.
    roxmltree::Document::parse(&xml).context("generated autounattend.xml doesn't parse")?;
    Ok(xml)
}

pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}
