//! Task Scheduler XML, for things that run elevated as the user: at logon
//! (rather than from Run keys, which explorer runs, and explorer isn't the
//! shell), or on demand (so the unelevated shell can start them without a
//! UAC prompt).

pub struct Task<'a> {
    pub description: &'a str,
    pub user: &'a str,
    pub trigger: Trigger,
    pub logon: Logon,
    pub command: &'a str,
    pub arguments: String,
}

pub enum Trigger {
    /// At each logon of the task's user.
    Logon,
    /// Only when started, by `schtasks /run` or its API. Task Scheduler
    /// lets the task's own user do that unelevated, which is how something
    /// non-elevated gets an elevated action without a UAC prompt.
    OnDemand,
}

/// Every task here runs elevated (HighestAvailable) as the configured user;
/// what differs is the session.
pub enum Logon {
    /// In the user's own session: its windows show on their desktop.
    Interactive,
    /// S4U: in session 0, so nothing it opens is ever seen (elevated windows
    /// starve the shell's keyboard hook), and no stored password.
    Session0,
}

/// The XML, UTF-16 with a BOM: schtasks takes nothing else. Its defaults
/// would skip a task on battery and stop it after 72 hours, hence the
/// settings.
pub fn xml(t: &Task) -> anyhow::Result<Vec<u8>> {
    let Task { description, user, trigger, logon, command, arguments } = t;
    let triggers = match trigger {
        Trigger::Logon => format!(
            "\n  <Triggers>\n    <LogonTrigger>\n      <Enabled>true</Enabled>\n      <UserId>{}</UserId>\n    </LogonTrigger>\n  </Triggers>",
            crate::unattend::escape(user)
        ),
        Trigger::OnDemand => String::new(),
    };
    let logon_type = match logon {
        Logon::Interactive => "InteractiveToken",
        Logon::Session0 => "S4U",
    };
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>{description}</Description>
  </RegistrationInfo>{triggers}
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId><LogonType>{logon_type}</LogonType><RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <ExecutionTimeLimit>PT1H</ExecutionTimeLimit>
    <Priority>5</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{command}</Command>
      <Arguments>{arguments}</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
        description = crate::unattend::escape(description),
        user = crate::unattend::escape(user),
        arguments = crate::unattend::escape(arguments),
    )
    .replace('\n', "\r\n");
    roxmltree::Document::parse(&xml.replace("encoding=\"UTF-16\"", "encoding=\"UTF-8\""))
        .map_err(|e| anyhow::anyhow!("generated task XML doesn't parse: {e}"))?;
    Ok(crate::registry::utf16(&xml))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(trigger: Trigger, logon: Logon) -> String {
        let t = Task { description: "d", user: "u", trigger, logon, command: "c.exe", arguments: "-x".into() };
        let bytes = xml(&t).unwrap();
        let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect();
        String::from_utf16(&units).unwrap()
    }

    #[test]
    fn on_demand_has_no_trigger() {
        let s = text(Trigger::OnDemand, Logon::Session0);
        assert!(!s.contains("<Triggers>"), "{s}");
        assert!(s.contains("<LogonType>S4U</LogonType><RunLevel>HighestAvailable</RunLevel>"), "{s}");
        assert!(s.contains("<AllowStartOnDemand>true</AllowStartOnDemand>"), "{s}");
    }

    #[test]
    fn logon_trigger_names_the_user() {
        let s = text(Trigger::Logon, Logon::Interactive);
        assert!(s.contains("<LogonTrigger>\r\n      <Enabled>true</Enabled>\r\n      <UserId>u</UserId>"), "{s}");
        assert!(s.contains("<LogonType>InteractiveToken</LogonType>"), "{s}");
    }
}
