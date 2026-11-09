//! Task Scheduler XML, for things that run at the user's logon. Logon tasks
//! rather than Run keys: explorer is what runs those, and explorer isn't the
//! shell.

pub struct LogonTask<'a> {
    pub description: &'a str,
    pub user: &'a str,
    /// `<UserId>…</UserId><LogonType>…` etc.
    pub principal: String,
    pub command: &'a str,
    pub arguments: String,
}

/// The XML, UTF-16 with a BOM: schtasks takes nothing else. Its defaults
/// would skip a task on battery and stop it after 72 hours, hence the
/// settings.
pub fn xml(t: &LogonTask) -> anyhow::Result<Vec<u8>> {
    let LogonTask { description, user, principal, command, arguments } = t;
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>{description}</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      {principal}
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
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
        arguments = crate::unattend::escape(arguments),
    )
    .replace('\n', "\r\n");
    roxmltree::Document::parse(&xml.replace("encoding=\"UTF-16\"", "encoding=\"UTF-8\""))
        .map_err(|e| anyhow::anyhow!("generated task XML doesn't parse: {e}"))?;
    Ok(crate::registry::utf16(&xml))
}
